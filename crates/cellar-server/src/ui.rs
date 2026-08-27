//! The web UI, assembled at compile time.
//!
//! Three files embedded in the binary and stitched together once: a server
//! manager that needs a node toolchain to render its own status page has a
//! second thing to keep working, and it is always the one that breaks during an
//! incident.
//!
//! The palette is not written into the stylesheet. It is generated from
//! `cellar_core::theme`, which states the Applejack tokens once, so a brand
//! change upstream is one edit rather than a search through CSS.

use std::sync::Arc;
use std::sync::OnceLock;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use cellar_core::config::WebAuthMode;

use crate::session::{self, COOKIE};
use crate::state::AppState;

const HTML: &str = include_str!("ui/index.html");
const CSS: &str = include_str!("ui/style.css");
const JS: &str = include_str!("ui/app.js");
const SERVICE_WORKER: &str = include_str!("ui/service-worker.js");
const FAVICON: &str = include_str!("ui/assets/favicon.svg");
const APP_ICON: &str = include_str!("ui/assets/cellar-icon.svg");
const AUTH_SLOT: &str = r#"<div id="auth-reminder" class="security-banner" hidden></div>"#;
const MANIFEST: &str = r##"{"name":"Cellar","short_name":"Cellar","start_url":"/","display":"standalone","theme_color":"#0E0F11","background_color":"#0E0F11","icons":[{"src":"/cellar-icon.svg","sizes":"192x192","type":"image/svg+xml","purpose":"any maskable"},{"src":"/cellar-icon.svg","sizes":"512x512","type":"image/svg+xml","purpose":"any maskable"}],"shortcuts":[{"name":"Dispatch","url":"/?tab=dispatch"},{"name":"Monitoring","url":"/?tab=monitoring"}],"description":"The dedicated server manager built for s&box."}"##;

/// The finished page, built once.
fn page() -> &'static str {
    static PAGE: OnceLock<String> = OnceLock::new();
    PAGE.get_or_init(|| {
        HTML.replace("/*PALETTE*/", &cellar_core::theme::css_variables())
            .replace("/*STYLE*/", CSS)
            .replace("/*APP*/", JS)
    })
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(index))
        .route("/favicon.svg", get(favicon))
        .route("/cellar-icon.svg", get(app_icon))
        .route("/service-worker.js", get(service_worker))
        .route("/manifest.webmanifest", get(manifest))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
}

async fn index(State(state): State<Arc<AppState>>) -> Response {
    let page = page_for_state(&state);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // Everything is inline and same-origin, so the policy can be strict.
            // `unsafe-inline` is needed only because the page is one file.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; \
                 connect-src 'self'; img-src 'self' data:; form-action 'self'; frame-ancestors 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        page,
    )
        .into_response()
}

fn page_for_state(state: &AppState) -> String {
    page().replace(AUTH_SLOT, &auth_notice(state))
}

fn auth_notice(state: &AppState) -> String {
    if !state.web_enabled {
        return String::new();
    }

    let reachable = !cellar_core::config::binds_loopback(&state.web_bind);
    let password_ready = state.web_password_hash.is_some();
    let password_required = state.web_auth == WebAuthMode::Password
        || (state.web_auth == WebAuthMode::Auto && password_ready);

    let (class, title, message) = if reachable && (!password_required || !state.web_secure_cookies)
    {
        (
            "security-banner danger",
            "Web UI security needs attention",
            "This listener is reachable beyond this PC. Configure password authentication, put it behind HTTPS, and enable secure cookies before sharing the address.",
        )
    } else if reachable {
        (
            "security-banner warning",
            "Web UI is remotely reachable",
            "Password authentication is configured. Keep this listener behind HTTPS and share its address only with trusted operators.",
        )
    } else if !password_required {
        (
            "security-banner warning",
            "Web UI is local-only without a password",
            "Keep the listener bound to loopback. Add CELLAR_WEB_PASSWORD_HASH if this address may become reachable from another device.",
        )
    } else {
        return String::new();
    };

    format!(
        r#"<aside id="auth-reminder" class="{class}" role="alert"><strong>{title}</strong><span>{message}</span></aside>"#
    )
}

async fn favicon() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        FAVICON,
    )
        .into_response()
}

async fn app_icon() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        APP_ICON,
    )
        .into_response()
}

async fn service_worker() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        SERVICE_WORKER,
    )
        .into_response()
}

async fn manifest() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/manifest+json")],
        MANIFEST,
    )
        .into_response()
}

#[derive(Deserialize)]
struct Login {
    password: String,
}

async fn login(State(state): State<Arc<AppState>>, Json(login): Json<Login>) -> Response {
    let Some(hash) = &state.web_password_hash else {
        if state.web_auth == WebAuthMode::Password {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(
                    serde_json::json!({ "ok": false, "error": "password auth is not configured" }),
                ),
            )
                .into_response();
        }
        return Json(serde_json::json!({ "ok": true })).into_response();
    };

    if !state.login_limiter.allow() {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            Json(serde_json::json!({ "ok": false, "error": "too many login attempts" })),
        )
            .into_response();
    }

    if !session::verify_password(&login.password, hash.expose()) {
        state.login_limiter.record_failure();
        // No detail: "wrong password" and "no operator configured" must look the
        // same from outside.
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "ok": false })),
        )
            .into_response();
    }

    let token = state.sessions.create("operator");

    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            if state.web_secure_cookies {
                format!(
                    "{COOKIE}={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=43200"
                )
            } else {
                format!("{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200")
            },
        )],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

async fn logout(State(state): State<Arc<AppState>>, headers: axum::http::HeaderMap) -> Response {
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for pair in cookie.split(';') {
            if let Some((key, value)) = pair.split_once('=')
                && key.trim() == COOKIE
            {
                state.sessions.destroy(value.trim());
            }
        }
    }

    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            if state.web_secure_cookies {
                format!("{COOKIE}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0")
            } else {
                format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
            },
        )],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_page_is_assembled_with_no_placeholders_left() {
        let page = page();
        assert!(!page.contains("/*PALETTE*/"));
        assert!(!page.contains("/*STYLE*/"));
        assert!(!page.contains("/*APP*/"));
        assert!(page.contains(AUTH_SLOT));
    }

    #[test]
    fn the_page_recommends_auth_for_a_reachable_listener() {
        let mut state = AppState::new(
            crate::state::Documents::memory(),
            crate::auth::Policy::Trusted,
            "test",
        );
        state.web_enabled = true;
        state.web_bind = "0.0.0.0:8081".to_owned();
        let page = page_for_state(&state);
        assert!(page.contains("Web UI security needs attention"));
        assert!(page.contains("Configure password authentication"));
    }

    #[test]
    fn the_page_warns_about_a_local_unauthenticated_listener() {
        let mut state = AppState::new(
            crate::state::Documents::memory(),
            crate::auth::Policy::Trusted,
            "test",
        );
        state.web_enabled = true;
        assert!(page_for_state(&state).contains("Web UI is local-only without a password"));
    }

    #[test]
    fn the_manifest_points_to_local_installable_icons() {
        let _: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        assert!(MANIFEST.contains("/cellar-icon.svg"));
        assert!(!MANIFEST.contains("http://"));
        assert!(!MANIFEST.contains("https://"));
        assert!(FAVICON.contains("<svg"));
        assert!(APP_ICON.contains("<svg"));
    }

    #[test]
    fn the_palette_reaches_the_page_from_the_theme_module() {
        let page = page();
        // Applejack is blue by standing rule; the page must be serving that blue
        // and not a hex somebody typed into the stylesheet.
        assert!(page.contains("--aj-azure: #2F8FE0"));
        assert!(page.contains("--aj-russet: #DA5B4D"));
        assert!(page.contains("--aj-orchard: #6FA862"));
    }

    #[test]
    fn no_colour_is_hardcoded_in_the_stylesheet() {
        // Every colour must arrive as a custom property. A literal hex here is a
        // second copy of the palette, and BRANDING.md is explicit that a second
        // copy is one that goes stale.
        let offenders: Vec<&str> = CSS
            .lines()
            .filter(|line| line.contains('#') && !line.trim_start().starts_with('*'))
            .filter(|line| {
                line.split('#')
                    .skip(1)
                    .any(|rest| rest.chars().take(3).all(|c| c.is_ascii_hexdigit()))
            })
            .collect();

        assert!(offenders.is_empty(), "hardcoded colours: {offenders:?}");
    }

    #[test]
    fn the_page_loads_nothing_from_the_network() {
        // A dashboard for a server that is down must not need a CDN to render.
        assert!(!HTML.contains("http://"));
        assert!(!HTML.contains("//cdn"));
        assert!(!HTML.contains("<script src"));
        assert!(!HTML.contains("<link rel=\"stylesheet\""));
    }

    #[test]
    fn the_script_never_assigns_untrusted_text_as_markup() {
        // A player's display name reaches this page through the log. Rendering
        // it as HTML would be stored cross-site scripting with a Steam profile
        // as the input field.
        assert!(!JS.contains("innerHTML"));
        assert!(!JS.contains("outerHTML"));
        assert!(!JS.contains("insertAdjacentHTML"));
        assert!(!JS.contains("document.write"));
    }

    #[test]
    fn every_tab_in_the_nav_has_a_section() {
        let tabs: Vec<&str> = HTML
            .split("data-tab=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();

        assert!(!tabs.is_empty());
        for tab in tabs {
            assert!(
                HTML.contains(&format!("id=\"tab-{tab}\"")),
                "no section for {tab}"
            );
        }
    }
}

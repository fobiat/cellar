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
const MANIFEST: &str = r##"{"name":"Cellar","short_name":"Cellar","start_url":"/","display":"standalone","theme_color":"#0E0F11","background_color":"#0E0F11","icons":[{"src":"/cellar-icon.svg","sizes":"192x192","type":"image/svg+xml","purpose":"any maskable"},{"src":"/cellar-icon.svg","sizes":"512x512","type":"image/svg+xml","purpose":"any maskable"}],"shortcuts":[{"name":"Dispatch","url":"/#/dispatch"},{"name":"Monitoring","url":"/#/monitoring"}],"description":"The dedicated server manager built for s&box."}"##;

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
        assert!(page.contains("id=\"header-profile\""));
        assert!(page.contains("id=\"header-restart\""));
        assert!(page.contains("id=\"header-build\""));
        assert!(page.contains("LIVE - DEVELOPMENT"));
        assert!(page.contains("AUTO-RESTART ON CRASH"));
        assert!(page.contains("id=\"config-mode-actions\""));
        assert!(page.contains("Development mode"));
        assert!(page.contains("Published mode"));
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

    /// Every control an operator types into has to say what it is for.
    ///
    /// There were zero `<label>` elements. Every input relied on a
    /// `placeholder`, which disappears on focus, so tabbing into the allowlist
    /// box gave an empty field with no clue what belonged in it, and no
    /// accessible name at all.
    #[test]
    fn every_control_has_an_accessible_name() {
        let named: Vec<&str> = HTML
            .split('<')
            .filter(|tag| {
                tag.starts_with("input") || tag.starts_with("select") || tag.starts_with("textarea")
            })
            // A submit button and a hidden field are not things anybody types
            // a value into looking for a hint.
            .filter(|tag| !tag.contains("type=\"file\"") || tag.contains("id="))
            .filter(|tag| !tag.contains("aria-label="))
            .collect();

        assert!(named.len() >= 20, "only found {} controls", named.len());

        for tag in named {
            let Some(id) = tag
                .split("id=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
            else {
                panic!("a control with neither an id nor an aria-label: <{tag}");
            };
            assert!(
                HTML.contains(&format!("for=\"{id}\"")),
                "no label points at '{id}', so it has no accessible name"
            );
        }
    }

    /// The tab bar has to be a tab bar to anything that is not a mouse.
    ///
    /// `aria-selected` was set on bare `<button>` elements, which means nothing
    /// without the roles around it, and there was no `tabindex` anywhere in the
    /// page, so eleven tabs were eleven separate stops before any content.
    #[test]
    fn the_tab_bar_follows_the_tabs_pattern() {
        assert!(HTML.contains(r#"<nav class="tabs" role="tablist""#));

        let tabs: Vec<&str> = HTML
            .split('<')
            .filter(|tag| tag.starts_with("button role=\"tab\""))
            .collect();
        assert!(tabs.len() >= 12, "found {} tabs", tabs.len());

        for tab in &tabs {
            let name = tab
                .split("data-tab=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_else(|| panic!("a tab with no data-tab: <{tab}"));
            assert!(
                tab.contains(&format!("aria-controls=\"tab-{name}\"")),
                "the '{name}' tab does not say which panel it controls"
            );
            assert!(
                tab.contains("tabindex="),
                "the '{name}' tab is not part of the roving tabindex"
            );
            assert!(
                HTML.contains(&format!(
                    r#"<section id="tab-{name}" role="tabpanel" aria-labelledby="tabfor-{name}""#
                )),
                "the '{name}' panel is not labelled by its tab"
            );
        }

        // Left, Right, Home and End, or the bar is a mouse-only control.
        for key in ["ArrowLeft", "ArrowRight", "Home", "End"] {
            assert!(JS.contains(key), "the tab bar does not handle {key}");
        }
    }

    /// A status must never be carried by colour alone.
    ///
    /// Every lamp was the same filled circle in a different hue, so in
    /// greyscale, or to a red-green colour deficiency, "running" and "crashed"
    /// were the same picture.
    #[test]
    fn no_status_is_carried_by_colour_alone() {
        for state in ["up", "down", "wait", "warn", "live"] {
            assert!(
                CSS.contains(&format!(".{state}::before")),
                "the '{state}' lamp has no glyph of its own"
            );
        }

        // Two states sharing a glyph is the same defect with extra steps.
        let glyphs: Vec<&str> = CSS
            .lines()
            .filter(|line| {
                ["up", "down", "wait", "warn", "live"]
                    .iter()
                    .any(|state| line.starts_with(&format!(".{state}::before")))
            })
            .filter_map(|line| line.split("content: \"").nth(1)?.split('"').next())
            .collect();
        assert_eq!(glyphs.len(), 5, "found {glyphs:?}");
        let mut unique = glyphs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            glyphs.len(),
            unique.len(),
            "two lamps share a glyph: {glyphs:?}"
        );
    }

    /// A destructive action gets a dialog that can name what it is about.
    ///
    /// `window.confirm` cannot say which server, cannot count who is about to
    /// be disconnected, and can be suppressed permanently by the browser, which
    /// turns "really stop the production server?" into a silent yes.
    #[test]
    fn destructive_actions_do_not_use_window_confirm() {
        let offenders: Vec<&str> = JS
            .lines()
            .map(str::trim)
            .filter(|line| line.contains("confirm(") && !line.contains("confirmAction("))
            .filter(|line| !line.trim_start().starts_with('*'))
            .collect();
        assert!(
            offenders.is_empty(),
            "window.confirm survives at: {offenders:?}"
        );

        assert!(HTML.contains(r#"<dialog id="confirm-dialog""#));
        assert!(
            JS.contains("dialog.showModal()"),
            "the dialog is in the markup and never opened"
        );

        // Measured, not assumed: the `close` event does not fire in every
        // engine that ships <dialog>, so a confirmation that resolves from it
        // hangs and the confirmed action silently never runs.
        assert!(
            !JS.contains("dialog.onclose") && !JS.contains(r#"addEventListener("close""#),
            "the confirmation must not depend on the dialog's close event"
        );
    }

    /// The endpoints that existed with no way to reach them.
    #[test]
    fn every_endpoint_the_ui_owns_has_a_control() {
        for (route, what) in [
            ("/api/logout", "signing out"),
            ("/api/control/exit", "shutting Cellar down"),
            ("method: \"DELETE\"", "deleting a document"),
        ] {
            assert!(JS.contains(route), "no way to reach {what} from the UI");
        }
    }

    /// The UI half of the AppleJackRP coupling, pinned.
    ///
    /// The Precinct tab was thirteen `data-command="applejack_*"` buttons in
    /// markup, so every other gamemode's operator got a panel of commands their
    /// server would reject. They come from `[[profile.command]]` now, and the
    /// only `applejack` left anywhere in the assets should be prose explaining
    /// that it is an example.
    #[test]
    fn the_page_hardcodes_no_gamemode_commands() {
        let offenders: Vec<&str> = HTML
            .lines()
            .chain(JS.lines())
            .filter(|line| line.contains("applejack_"))
            .collect();

        assert!(
            offenders.is_empty(),
            "a gamemode's commands belong in its profile, not in the assets: {offenders:?}"
        );
    }

    /// Every route that is about one supervised server must carry the
    /// instance, or a two-server dashboard silently answers about the primary.
    ///
    /// The check is that no bare literal survives, rather than that
    /// `forInstance` is called some number of times: a new call site added
    /// later fails this without anyone having to remember the rule.
    #[test]
    fn every_instance_scoped_fetch_names_its_instance() {
        const SCOPED: [&str; 8] = [
            "/api/status",
            "/api/exec",
            "/api/control/",
            "/api/logs",
            "/api/access",
            "/api/settings",
            "/api/docs",
            "/api/settings/import",
        ];

        let offenders: Vec<String> = JS
            .lines()
            .map(str::trim)
            .filter(|line| line.contains("fetch(") && !line.contains("forInstance("))
            // `control/exit` is the one control action that is about the
            // process rather than about a server: the handler ignores the
            // target and stops every instance. Naming one would be a lie.
            .filter(|line| !line.contains("/api/control/exit"))
            .filter(|line| SCOPED.iter().any(|route| line.contains(route)))
            .map(str::to_owned)
            .collect();

        assert!(
            offenders.is_empty(),
            "these fetches are about one server and do not say which: {offenders:?}"
        );
    }

    /// Tab state was a JS variable, which is why both PWA manifest shortcuts
    /// landed on the same screen and a reload lost the tab. It is the location
    /// hash now, and the manifest has to point at hashes for that to help.
    #[test]
    fn the_manifest_shortcuts_are_routes_rather_than_the_same_screen_twice() {
        let parsed: serde_json::Value =
            serde_json::from_str(MANIFEST).expect("the manifest is valid JSON");
        let targets: Vec<&str> = parsed["shortcuts"]
            .as_array()
            .expect("shortcuts is an array")
            .iter()
            .map(|entry| entry["url"].as_str().expect("a shortcut has a url"))
            .collect();

        assert!(targets.len() >= 2, "fewer than two shortcuts");
        for target in &targets {
            assert!(target.contains('#'), "{target} is not a route");
        }
        assert_eq!(
            targets
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            targets.len(),
            "two shortcuts point at the same screen: {targets:?}"
        );
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

    /// The browser drops any event kind it does not name, silently. That is how
    /// the whole shutdown transcript, which `graceful_stop` publishes as
    /// `Unparsed`, went unrendered: a clean stop could not be watched from the
    /// web UI at all.
    #[test]
    fn the_script_handles_every_event_kind_the_server_can_send() {
        // Read from `Event::kind` rather than listed here. A hand-written list
        // can only catch a variant somebody remembered to add to it, which is
        // exactly how `command_dispatched` and `command_replied` were broadcast
        // and dropped by the browser for months while this test passed.
        const EVENT_SOURCE: &str = include_str!("../../cellar-core/src/event.rs");

        let kinds: Vec<&str> = EVENT_SOURCE
            .lines()
            .skip_while(|line| !line.contains("pub fn kind(&self)"))
            .take_while(|line| !line.trim_start().starts_with("/// Whether this is worth"))
            .filter_map(|line| line.split_once("=> \"")?.1.split('"').next())
            .collect();
        assert!(kinds.len() >= 12, "only found {kinds:?}");

        for kind in kinds {
            // The two high-frequency samples. A console that printed a resource
            // sample twice a second would be a console nobody could read.
            if matches!(kind, "status" | "resources") {
                continue;
            }
            assert!(
                JS.contains(&format!("case \"{kind}\":")),
                "app.js drops the '{kind}' event"
            );
        }

        // Synthesised by ws.rs rather than being `Event` variants, so they are
        // not in the enum and still have to be handled.
        for kind in ["notice", "lagged"] {
            assert!(
                JS.contains(&format!("case \"{kind}\":")),
                "app.js drops the '{kind}' notice"
            );
        }
    }

    /// Every verdict the diagnostics crate can return has to render as a word.
    ///
    /// Read from the enum rather than listed here, for the same reason the
    /// event-kind test above is: a hand-written list only catches the variants
    /// somebody remembered to add to it.
    #[test]
    fn the_script_renders_every_diagnostic_outcome() {
        const SOURCE: &str = include_str!("../../cellar-diagnostics/src/lib.rs");

        let outcomes: Vec<String> = SOURCE
            .lines()
            .skip_while(|line| !line.contains("pub enum Outcome"))
            .take_while(|line| !line.starts_with('}'))
            .filter_map(|line| {
                let word = line.trim().trim_end_matches(',');
                word.chars()
                    .all(|character| character.is_ascii_alphabetic())
                    .then(|| word.to_lowercase())
            })
            .filter(|word| !word.is_empty())
            .collect();
        assert_eq!(outcomes, ["ok", "fail", "note"], "found {outcomes:?}");

        for outcome in outcomes {
            assert!(
                JS.contains(&format!("{outcome}:")),
                "app.js has no word for the '{outcome}' outcome, so it would render as a colour \
                 alone"
            );
        }
    }

    /// The doctor checks exist once.
    ///
    /// They used to live in `cellar-cli` and print as they went, which put the
    /// dashboard one crate boundary away from reaching them. Reimplementing
    /// them in the server was the option to refuse: a second copy of a check is
    /// a second copy that drifts.
    #[test]
    fn the_server_does_not_reimplement_the_doctor_checks() {
        const API: &str = include_str!("api.rs");

        assert!(
            API.contains("cellar_diagnostics::run("),
            "the diagnostics route must call the shared crate"
        );
        for smell in [
            "appmanifest_",
            "no dotnet.exe",
            "without loading the gamemode",
        ] {
            assert!(
                !API.contains(smell),
                "api.rs has grown its own copy of the '{smell}' check"
            );
        }
    }

    /// One arriving line must cost one DOM node, not a full teardown.
    ///
    /// `appendLine` used to call `renderConsole`, which called
    /// `replaceChildren` and rebuilt up to 1500 elements per line. That is
    /// O(n) work per line, and it is what "slow mode" existed to hide.
    #[test]
    fn an_arriving_console_line_does_not_redraw_the_whole_console() {
        // Bounded at the function's own closing brace, which in this file is
        // the first `}` in column zero. Splitting on the next `function`
        // keyword ran past the end into `renderConsole`, which legitimately
        // does redraw everything.
        let append = JS
            .split_once("function appendLine(")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("appendLine is defined");

        assert!(
            !append.contains("renderConsole("),
            "appendLine redraws the whole console for every line"
        );
        assert!(
            !append.contains("replaceChildren"),
            "appendLine tears the console down for every line"
        );
        assert!(
            append.contains("console_.append("),
            "appendLine should append one node"
        );

        // The control that existed to work around the cost, and its state.
        assert!(!JS.contains("consoleSlow"), "slow mode should be gone");
        assert!(
            !HTML.contains("console-slow"),
            "the slow mode button should be gone"
        );
    }

    /// The categorisation rule lives in the gamemode profile, in Rust. A second
    /// copy in JavaScript had already drifted: it still tested for `applejack`
    /// after the Rust side started asking the profile.
    #[test]
    fn the_browser_does_not_reimplement_log_categorisation() {
        assert!(
            !JS.contains("function logCategory("),
            "the category rule belongs to the profile, not to app.js"
        );
    }

    /// A command must appear once, not twice.
    ///
    /// `command_dispatched` and `command_replied` are broadcast to every
    /// browser. Rendering the HTTP reply locally as well showed the same reply
    /// twice, which is what handling those two events for the first time
    /// exposed. Rendering only locally would hide every command the CLI, MCP or
    /// another operator ran, which is the reason to handle them at all.
    #[test]
    fn a_command_reply_is_rendered_once() {
        let run = JS
            .split_once("async function runCommand(")
            .and_then(|(_, rest)| rest.split_once("\n}\n"))
            .map(|(body, _)| body)
            .expect("runCommand is defined");

        assert!(
            run.contains("echoLocally"),
            "runCommand renders unconditionally, so a broadcast reply lands twice"
        );
        assert!(
            JS.contains("function commandsArriveOnTheStream("),
            "nothing decides which of the two sources renders"
        );
    }

    /// A gap in the console must be a gap, not a line that reads like engine
    /// output the grammar failed on.
    #[test]
    fn the_lag_notice_is_not_dressed_up_as_an_unparsed_line() {
        const WS: &str = include_str!("ws.rs");
        assert!(WS.contains("\"kind\": \"lagged\""));
        assert!(
            !WS.contains("\"kind\": \"unparsed\""),
            "ws.rs is synthesising an unparsed event again"
        );
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

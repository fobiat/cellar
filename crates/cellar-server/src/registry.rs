//! Every supervised server this process owns.
//!
//! Fixed at startup, on purpose. Instances are declared in `cellar.toml` and the
//! set does not change while Cellar runs, so this is a slice rather than a
//! locked map. A registry that can grow needs a lifecycle, an id allocator and a
//! create route, and this product's instances are the development, staging and
//! production of one gamemode rather than tenants.

use std::path::PathBuf;
use std::sync::Arc;

use cellar_core::config::{Instance, InstanceId};
use cellar_runtime::Handle;
use serde::Serialize;

/// What the routes need to know about one instance without reading its config.
///
/// A snapshot taken at startup rather than a borrow of the config, because
/// `SwitchConfig` can replace a supervisor's config underneath it and a route
/// reading a stale field is better than a route holding a lock.
#[derive(Debug, Clone, Serialize)]
pub struct Descriptor {
    pub log_file: Option<PathBuf>,
    pub game: Option<String>,
    pub map: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub port: u16,
    pub query_port: u16,
    pub direct_connect: bool,
    pub bridge_bind: String,
    pub bridge_enabled: bool,
}

/// One instance, running or not.
#[derive(Clone)]
pub struct Entry {
    pub id: InstanceId,
    /// The storage key, which is not the id. See `Config::instances`.
    pub scope: String,
    /// Absent when the instance is declared but not started here. `unavailable`
    /// is why, and it is what the dashboard shows in place of a state.
    pub handle: Option<Handle>,
    pub unavailable: Option<String>,
    /// Whether `/readyz` speaks for this instance.
    pub required: bool,
    pub descriptor: Descriptor,
}

impl Entry {
    /// Build the parts that come from the config. The handle is attached once
    /// the supervisor for it exists.
    pub fn from_instance(instance: &Instance) -> Self {
        Self {
            id: instance.id.clone(),
            scope: instance.scope.clone(),
            handle: None,
            unavailable: (!instance.enabled)
                .then(|| "declared but not enabled in this config".to_owned()),
            required: instance.required,
            descriptor: Descriptor {
                log_file: Some(instance.server.engine_log_file()),
                game: instance.server.game.clone(),
                map: instance.server.map.clone(),
                data_dir: instance.server.game_data_dir(),
                port: instance.server.port,
                query_port: instance.server.query_port,
                direct_connect: instance.server.direct_connect,
                bridge_bind: instance.bridge.bind.clone(),
                bridge_enabled: instance.bridge.enabled,
            },
        }
    }
}

/// The instances, in id order, with one of them nominated as the default.
#[derive(Clone)]
pub struct Registry {
    entries: Arc<[Entry]>,
    primary: usize,
}

impl Registry {
    /// Build from the desugared config. `primary` is the first enabled entry,
    /// matching `Config::primary`, so an unqualified request means the same
    /// instance everywhere.
    pub fn new(entries: Vec<Entry>) -> Self {
        let primary = entries
            .iter()
            .position(|entry| entry.unavailable.is_none())
            .unwrap_or(0);
        Self {
            entries: entries.into(),
            primary,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// The instance an unqualified request means.
    pub fn primary(&self) -> Option<&Entry> {
        self.entries.get(self.primary)
    }

    /// Look one up by id.
    ///
    /// Returns `None` rather than falling back to the primary. A typo'd id must
    /// never quietly run `quit` against production.
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id.as_str() == id)
    }

    /// The ids that do exist, for a 404 that helps.
    pub fn ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.id.as_str().to_owned())
            .collect()
    }

    /// Attach a running supervisor to one entry.
    pub fn set_handle(entries: &mut [Entry], id: &InstanceId, handle: Handle) {
        if let Some(entry) = entries.iter_mut().find(|entry| &entry.id == id) {
            entry.handle = Some(handle);
            entry.unavailable = None;
        }
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// The instance a request is about.
///
/// `?instance=<id>`, defaulting to the primary, rather than a path prefix. The
/// reason is compatibility: `cellar-mcp` calls six routes, the CLI's live-server
/// commands call two more, and `app.js` calls about twenty-five. A prefix means
/// changing all of them to gain nothing a parameter does not already give, and
/// Cellar's authorization is a per-handler extractor rather than a path policy,
/// so nothing depends on the instance being in the path.
///
/// An unknown id is a 404 naming the ids that do exist. It is never a silent
/// fallback to the primary, because the request that gets misrouted that way is
/// `quit`.
pub struct Target(pub Entry);

impl std::ops::Deref for Target {
    type Target = Entry;

    fn deref(&self) -> &Entry {
        &self.0
    }
}

impl<S> axum::extract::FromRequestParts<S> for Target
where
    S: Send + Sync,
    Arc<crate::state::AppState>: axum::extract::FromRef<S>,
{
    type Rejection = (axum::http::StatusCode, String);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        use axum::extract::FromRef;
        let state = Arc::<crate::state::AppState>::from_ref(state);

        let requested = parts.uri.query().and_then(|query| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == "instance").then(|| value.to_owned())
            })
        });

        match requested {
            Some(id) => state
                .instances
                .get(&id)
                .cloned()
                .map(Target)
                .ok_or_else(|| {
                    (
                        axum::http::StatusCode::NOT_FOUND,
                        format!(
                            "no instance '{id}'. This config declares: {}",
                            state.instances.ids().join(", ")
                        ),
                    )
                }),
            None => state
                .instances
                .primary()
                .cloned()
                .map(Target)
                .ok_or_else(|| {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "no server is being supervised".to_owned(),
                    )
                }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn entry(id: &str, available: bool) -> Entry {
        Entry {
            id: InstanceId::new(id).unwrap(),
            scope: id.to_owned(),
            handle: None,
            unavailable: (!available).then(|| "not enabled".to_owned()),
            required: true,
            descriptor: Descriptor {
                log_file: None,
                game: None,
                map: None,
                data_dir: None,
                port: 27015,
                query_port: 27016,
                direct_connect: false,
                bridge_bind: "127.0.0.1:8080".to_owned(),
                bridge_enabled: false,
            },
        }
    }

    #[test]
    fn the_primary_skips_an_instance_that_is_not_running_here() {
        // A development instance that cannot start on a Linux host must not be
        // what an unqualified request lands on.
        let registry = Registry::new(vec![entry("dev", false), entry("published", true)]);

        assert_eq!(registry.primary().unwrap().id.as_str(), "published");
    }

    #[test]
    fn an_unknown_id_is_a_miss_rather_than_the_primary() {
        // The whole reason `get` returns Option: a typo'd id resolving to the
        // primary means `quit` reaching production.
        let registry = Registry::new(vec![entry("dev", true)]);

        assert!(registry.get("devv").is_none());
        assert_eq!(registry.ids(), vec!["dev".to_owned()]);
    }
}

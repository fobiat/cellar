//! Safe profile discovery and switching for a running Cellar instance.

use std::path::{Path, PathBuf};

use cellar_core::config::Config;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Profile {
    pub name: String,
    pub mode: &'static str,
    pub path: String,
    pub active: bool,
    pub game: Option<String>,
    pub project: String,
    pub map: Option<String>,

    /// What this profile would bind and read, so the dashboard can show why a switch is impossible rather than only that it was refused.
    pub web_bind: String,
    pub bridge_bind: String,
    pub log_file: String,

    /// Why switching to this profile would be refused, or `None`.
    /// Computed by the same function the switch route uses. Two copies of these rules would be two copies that drift, and the drift is invisible: the button would look enabled and the click would still fail.
    pub refusal: Option<String>,
}

/// What the running process is, for deciding whether a profile can replace it.
/// A borrowed shape rather than `&AppState` because the rules are about four values and nothing else, and passing the whole state invites a fifth rule that reads something no listing could have known.
pub struct Running {
    pub web_bind: String,
    pub web_enabled: bool,
    pub bridge_bind: Option<String>,
    pub bridge_enabled: bool,
    pub log_file: Option<PathBuf>,
    pub instances: usize,
    pub supervised: bool,
    pub active_config: Option<Config>,
}

/// Why this config cannot replace what is running, or `None` if it can.
/// Every refusal names the value that differs. "Profiles may change the supervised server, but not Cellar listener bindings" is true and does not tell an operator which of the four bindings they got wrong.
pub fn switch_refusal(candidate: &Config, running: &Running) -> Option<String> {
    // Ordered from the condition that makes the question meaningless to the one that is a fixable mistake in the file, so an operator reads the reason that is actually theirs to act on.
    if running.instances > 1 {
        return Some(format!(
            "this process supervises {} instances, so switching the whole profile is ambiguous. \
             Stop and start an instance instead.",
            running.instances
        ));
    }
    if !running.supervised {
        return Some("no server is being supervised".to_owned());
    }

    if candidate.web.enabled != running.web_enabled {
        return Some(format!(
            "this profile turns the web UI {}, and a profile may not change Cellar's own \
             listeners while it is running",
            if candidate.web.enabled { "on" } else { "off" }
        ));
    }
    if candidate.web.bind != running.web_bind {
        return Some(format!(
            "this profile binds the web UI to {} and Cellar is listening on {}",
            candidate.web.bind, running.web_bind
        ));
    }
    if candidate.bridge.enabled != running.bridge_enabled {
        return Some(format!(
            "this profile turns the document bridge {}, and a profile may not change Cellar's own \
             listeners while it is running",
            if candidate.bridge.enabled {
                "on"
            } else {
                "off"
            }
        ));
    }
    if running.bridge_enabled
        && Some(candidate.bridge.bind.as_str()) != running.bridge_bind.as_deref()
    {
        return Some(format!(
            "this profile binds the bridge to {} and Cellar is listening on {}",
            candidate.bridge.bind,
            running.bridge_bind.as_deref().unwrap_or("nothing")
        ));
    }

    let candidate_log = candidate.primary_server().unwrap_or_default();
    let candidate_log = candidate_log.engine_log_file();
    if running.log_file.as_deref() != Some(candidate_log.as_path()) {
        return Some(format!(
            "this profile reads {} and Cellar is tailing {}. The tailer is bound at startup.",
            candidate_log.display(),
            running
                .log_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "nothing".to_owned())
        ));
    }

    let Some(candidate_instance) = candidate.primary() else {
        return Some("that profile declares no server".to_owned());
    };

    if let Some(active) = &running.active_config {
        let Some(active_instance) = active.primary() else {
            return Some("the active profile no longer declares a server".to_owned());
        };
        if candidate_instance.id != active_instance.id {
            return Some(format!(
                "this profile names instance '{}' and the active profile names '{}'",
                candidate_instance.id, active_instance.id
            ));
        }
        if candidate_instance.scope != active_instance.scope {
            return Some(
                "this profile changes the bridge document scope, which is bound at startup"
                    .to_owned(),
            );
        }
        if candidate_instance.required != active_instance.required {
            return Some("this profile changes whether the instance is required for readiness, which is bound at startup".to_owned());
        }
        if candidate_instance.server.project != active_instance.server.project {
            return Some(
                "this profile changes server.project, which the update probe binds at startup"
                    .to_owned(),
            );
        }
        if different(&candidate_instance.bridge, &active_instance.bridge) {
            return Some(
                "this profile changes bridge policy, which is bound at startup".to_owned(),
            );
        }
        for (name, changed) in [
            ("web", different(&candidate.web, &active.web)),
            ("database", different(&candidate.database, &active.database)),
            (
                "notifications",
                different(&candidate.notify, &active.notify),
            ),
            ("update", different(&candidate.update, &active.update)),
            ("mariadb", different(&candidate.mariadb, &active.mariadb)),
            ("backup", different(&candidate.backup, &active.backup)),
            (
                "persistence",
                different(&candidate.persistence, &active.persistence),
            ),
            ("release", different(&candidate.release, &active.release)),
        ] {
            if changed {
                return Some(format!(
                    "this profile changes {name} policy, which is bound at startup"
                ));
            }
        }
    }

    None
}

fn different<T: Serialize>(left: &T, right: &T) -> bool {
    match (serde_json::to_value(left), serde_json::to_value(right)) {
        (Ok(left), Ok(right)) => left != right,
        _ => true,
    }
}

pub async fn list(
    directory: &Path,
    active: Option<&Path>,
    running: Option<&Running>,
) -> Vec<Profile> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return Vec::new();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| profile(path, active, running))
        .collect()
}

pub fn resolve(directory: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains(char::from(92))
        || name.contains(':')
        || name != name.trim()
    {
        return Err("profile names must be a simple TOML filename stem".to_owned());
    }
    let path = directory.join(format!("{name}.toml"));
    if !path.is_file() {
        return Err(format!("profile '{name}' does not exist"));
    }
    Ok(path)
}

pub fn load(path: &Path) -> Result<Config, String> {
    Config::load(path).map_err(|error| format!("could not load {}: {error}", path.display()))
}

fn profile(path: PathBuf, active: Option<&Path>, running: Option<&Running>) -> Option<Profile> {
    let config = Config::load(&path).ok()?;
    let server = config.primary_server()?;
    let is_active = active.is_some_and(|current| current == path.as_path());
    Some(Profile {
        name: path.file_stem()?.to_string_lossy().into_owned(),
        mode: if server.is_published() {
            "published"
        } else {
            "development"
        },
        path: path.to_string_lossy().into_owned(),
        active: is_active,
        game: server.game.clone(),
        project: server.project.to_string_lossy().into_owned(),
        map: server.map.clone(),
        web_bind: config.web.bind.clone(),
        bridge_bind: config.bridge.bind.clone(),
        log_file: server.engine_log_file().to_string_lossy().into_owned(),
        // The one already running is never offered as a switch, so a refusal on it would read as "this server is broken" rather than "you are here".
        refusal: if is_active {
            None
        } else {
            running.and_then(|running| switch_refusal(&config, running))
        },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config::parse_at(text, Path::new("/tmp/cellar.toml")).expect("parses")
    }

    fn base() -> Config {
        config(
            r#"
            [server]
            executable = "/srv/sbox/sbox-server.exe"
            project = "/srv/aj/applejackrp.sbproj"
            [web]
            enabled = true
            bind = "127.0.0.1:8080"
            [bridge]
            enabled = false
            "#,
        )
    }

    fn running(log: &Path, active_config: Config) -> Running {
        Running {
            web_bind: "127.0.0.1:8080".to_owned(),
            web_enabled: true,
            bridge_bind: None,
            bridge_enabled: false,
            log_file: Some(log.to_owned()),
            instances: 1,
            supervised: true,
            active_config: Some(active_config),
        }
    }

    #[test]
    fn a_profile_matching_what_is_running_is_switchable() {
        let candidate = base();
        let log = candidate.primary_server().unwrap().engine_log_file();
        assert_eq!(
            switch_refusal(&candidate, &running(&log, candidate.clone())),
            None
        );
    }

    /// The point of the whole change: the reason has to name the value that differs. "May not change Cellar listener bindings" was true for four different mistakes and told an operator which of them none of the time.
    #[test]
    fn a_refusal_names_the_value_that_differs() {
        let candidate = base();
        let log = candidate.primary_server().unwrap().engine_log_file();

        let mut elsewhere = running(&log, candidate.clone());
        elsewhere.web_bind = "0.0.0.0:9000".to_owned();
        let why = switch_refusal(&candidate, &elsewhere).expect("refused");
        assert!(
            why.contains("127.0.0.1:8080") && why.contains("0.0.0.0:9000"),
            "{why}"
        );

        let mut other_log = running(&log, candidate.clone());
        let elsewhere = PathBuf::from("/var/log/other.log");
        other_log.log_file = Some(elsewhere);
        let why = switch_refusal(&candidate, &other_log).expect("refused");
        assert!(why.contains("other.log"), "{why}");
    }

    /// Ambiguity beats a binding mismatch: with two instances the question has no answer, so saying the web bind is wrong would send an operator to fix a file that was never the problem.
    #[test]
    fn several_instances_is_reported_before_any_binding_difference() {
        let candidate = base();
        let log = candidate.primary_server().unwrap().engine_log_file();
        let mut two = running(&log, candidate.clone());
        two.instances = 2;
        two.web_bind = "0.0.0.0:9000".to_owned();

        let why = switch_refusal(&candidate, &two).expect("refused");
        assert!(why.contains("2 instances"), "{why}");
    }

    /// A disabled bridge has no address, so comparing one would refuse every profile in a bridge-free deployment for a difference that cannot matter.
    #[test]
    fn a_disabled_bridge_address_is_not_compared() {
        let candidate = config(
            r#"
            [server]
            executable = "/srv/sbox/sbox-server.exe"
            project = "/srv/aj/applejackrp.sbproj"
            [web]
            enabled = true
            bind = "127.0.0.1:8080"
            [bridge]
            enabled = false
            bind = "127.0.0.1:9999"
            "#,
        );
        let log = candidate.primary_server().unwrap().engine_log_file();
        assert_eq!(
            switch_refusal(&candidate, &running(&log, candidate.clone())),
            None
        );
    }

    #[test]
    fn server_identity_can_change_but_startup_policy_cannot() {
        let active = base();
        let log = active.primary_server().unwrap().engine_log_file();
        let mut candidate = active.clone();
        let server = candidate.server.as_mut().unwrap();
        server.game = Some("fobiat.other".to_owned());
        server.map = Some("other.map".to_owned());
        server.data_dir = Some(PathBuf::from("/srv/other/data"));
        assert_eq!(
            switch_refusal(&candidate, &running(&log, active.clone())),
            None
        );

        candidate.backup.enabled = true;
        let refusal = switch_refusal(&candidate, &running(&log, active)).unwrap();
        assert!(refusal.contains("backup policy"), "{refusal}");
    }
}

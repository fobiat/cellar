//! The hands-off updater.
//!
//! The whole risk here is one sentence: an updater that restarts a server with
//! people on it is worse than no updater. Everything below is arranged around
//! that, and the default policy is [`Policy::Notify`] rather than
//! [`Policy::Apply`], because taking an update is a decision an operator should
//! opt into rather than discover.
//!
//! When it does apply, the order matters. The engine's files are in use while it
//! runs and Steam cannot replace them underneath it, so the sequence is: decide
//! it is safe, stop gracefully with `quit` so the Steam logoff and the convar
//! save happen, update, and start again. Never update first and hope.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::version::{SBOX_DEDICATED_APP_ID, Versions};

/// The config lives in `cellar-core` so one `cellar.toml` deserialises whole.
pub use cellar_core::config::{UpdateConfig, UpdatePolicy as Policy};

/// Timing and window arithmetic over the shared config.
///
/// A free function rather than an inherent `impl`, because the type belongs to
/// another crate; the behaviour still belongs here, with its tests.
pub fn interval(config: &UpdateConfig) -> Duration {
    Duration::from_secs(config.check_interval_minutes.max(5) * 60)
}

/// Whether `hour` is inside the maintenance window.
///
/// Equal bounds mean "any time". A window that wraps midnight (22 to 4) is
/// supported, because that is when a roleplay server is actually empty.
pub fn in_window(config: &UpdateConfig, hour: u8) -> bool {
    let (start, end) = (config.window_start_hour % 24, config.window_end_hour % 24);
    if start == end {
        return true;
    }
    if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

/// Why an update was or was not applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Nothing to do.
    UpToDate,
    /// Something is available and the policy is to report it.
    Available { what: Vec<String> },
    /// Something is available but now is not the time.
    Deferred { what: Vec<String>, why: String },
    /// Take it.
    Apply { what: Vec<String> },
}

/// Decide what to do, given what is available and what the server is doing.
///
/// Pure, so every gate is a test rather than something discovered in production.
pub fn decide(
    config: &UpdateConfig,
    versions: &Versions,
    players_connected: usize,
    hour: u8,
) -> Decision {
    if config.policy == Policy::Off {
        return Decision::UpToDate;
    }

    let mut what = Vec::new();

    if let Some(git) = &versions.git
        && git.is_behind()
    {
        what.push(format!(
            "gamemode {} -> {}",
            git.short_head(),
            git.remote_head
                .as_deref()
                .map(|h| &h[..h.len().min(8)])
                .unwrap_or("?")
        ));
    }

    if let Some(engine) = &versions.engine
        && engine.is_behind()
    {
        what.push(format!(
            "engine build {} -> {}",
            engine.installed_build,
            engine.available_build.as_deref().unwrap_or("?")
        ));
    }

    if what.is_empty() {
        return Decision::UpToDate;
    }

    if config.policy == Policy::Notify {
        return Decision::Available { what };
    }

    // A dirty working tree means somebody is mid-edit. Pulling over that is how
    // an updater destroys work, so it refuses and says why.
    if let Some(git) = &versions.git
        && git.dirty
        && config.update_gamemode
    {
        return Decision::Deferred {
            what,
            why: "the gamemode checkout has uncommitted changes".to_owned(),
        };
    }

    if config.only_when_empty && players_connected > 0 {
        return Decision::Deferred {
            what,
            why: format!("{players_connected} player(s) are connected"),
        };
    }

    if !in_window(config, hour) {
        return Decision::Deferred {
            what,
            why: format!(
                "outside the maintenance window ({:02}:00 to {:02}:00)",
                config.window_start_hour, config.window_end_hour
            ),
        };
    }

    Decision::Apply { what }
}

/// What an apply actually did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Applied {
    pub steps: Vec<Step>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// Perform the update. The caller stops the server first and starts it after.
///
/// This does not touch the supervisor: sequencing the stop and the start around
/// it belongs to whoever owns the process, and an updater that could restart a
/// server on its own is an updater that will.
pub async fn apply(config: &UpdateConfig, project_dir: &std::path::Path) -> Applied {
    let mut steps = Vec::new();

    if config.update_gamemode {
        steps.push(pull(project_dir).await);
    }

    if config.update_engine {
        steps.push(steam_update(config).await);
    }

    let ok = steps.iter().all(|step| step.ok);
    Applied { steps, ok }
}

async fn pull(dir: &std::path::Path) -> Step {
    // `--ff-only`: a merge commit created unattended by a game server at 4am is
    // a merge nobody reviewed. If it will not fast-forward, that is a person's
    // problem to look at, not something to resolve automatically.
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["pull", "--ff-only"])
        .stdin(Stdio::null())
        .output()
        .await;

    match output {
        Ok(output) if output.status.success() => Step {
            name: "git pull --ff-only".to_owned(),
            ok: true,
            detail: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        },
        Ok(output) => Step {
            name: "git pull --ff-only".to_owned(),
            ok: false,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        },
        Err(error) => Step {
            name: "git pull --ff-only".to_owned(),
            ok: false,
            detail: error.to_string(),
        },
    }
}

async fn steam_update(config: &UpdateConfig) -> Step {
    let (Some(steamcmd), Some(steam_dir)) = (&config.steamcmd, &config.steam_dir) else {
        return Step {
            name: format!("steamcmd app_update {SBOX_DEDICATED_APP_ID}"),
            ok: false,
            detail: "update.steamcmd and update.steam_dir are both needed to update the engine"
                .to_owned(),
        };
    };
    install_engine(steamcmd, steam_dir, true, false).await
}

/// Download or update the dedicated server with steamcmd.
///
/// Shared by the update job and by `cellar install`, which is the whole
/// first-run path: **the dedicated server is app 1892930, it is free, and
/// anonymous login works**, so getting from nothing to an installed server
/// needs no Steam credential. App 590830 is the paid client and editor, and
/// anonymous fails on it with "No subscription".
///
/// `stream` prints steamcmd's own progress rather than swallowing it, because
/// this is a multi-gigabyte download and a command that prints nothing for
/// twenty minutes reads as a hang.
pub async fn install_engine(
    steamcmd: &std::path::Path,
    into: &std::path::Path,
    validate: bool,
    stream: bool,
) -> Step {
    let name = format!("steamcmd app_update {SBOX_DEDICATED_APP_ID}");

    let mut command = tokio::process::Command::new(steamcmd);
    command
        .args([
            "+@ShutdownOnFailedCommand",
            "1",
            "+@NoPromptForPassword",
            "1",
            // The dedicated server is a Windows binary. On Linux a plain
            // app_update takes the platform-neutral depots and silently skips
            // the one holding every .exe, which looks like a complete install
            // with no executable anywhere in it.
            "+@sSteamCmdForcePlatformType",
            "windows",
            "+force_install_dir",
        ])
        .arg(into)
        .args(["+login", "anonymous", "+app_update", SBOX_DEDICATED_APP_ID]);
    if validate {
        command.arg("validate");
    }
    command.arg("+quit").stdin(Stdio::null());

    if stream {
        return match command.status().await {
            Ok(status) if status.success() => Step {
                name,
                ok: true,
                detail: format!("installed into {}", into.display()),
            },
            Ok(status) => Step {
                name,
                ok: false,
                detail: format!("steamcmd exited with {status}"),
            },
            Err(error) => Step {
                name,
                ok: false,
                detail: error.to_string(),
            },
        };
    }

    match command.output().await {
        Ok(output) if output.status.success() => Step {
            name,
            ok: true,
            detail: last_lines(&String::from_utf8_lossy(&output.stdout), 3),
        },
        Ok(output) => Step {
            name,
            ok: false,
            detail: last_lines(&String::from_utf8_lossy(&output.stdout), 5),
        },
        Err(error) => Step {
            name,
            ok: false,
            detail: error.to_string(),
        },
    }
}

fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(count)..].join(" | ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::version::{EngineVersion, GitVersion};

    use super::*;

    fn behind() -> Versions {
        Versions {
            git: Some(GitVersion {
                head: "aaaaaaaaaaaa".to_owned(),
                remote_head: Some("bbbbbbbbbbbb".to_owned()),
                ..Default::default()
            }),
            engine: Some(EngineVersion {
                installed_build: "19551234".to_owned(),
                available_build: Some("19559999".to_owned()),
                reported: None,
            }),
            ..Default::default()
        }
    }

    fn applying() -> UpdateConfig {
        UpdateConfig {
            policy: Policy::Apply,
            ..Default::default()
        }
    }

    #[test]
    fn off_never_reports_anything() {
        let config = UpdateConfig {
            policy: Policy::Off,
            ..Default::default()
        };
        assert_eq!(decide(&config, &behind(), 0, 4), Decision::UpToDate);
    }

    #[test]
    fn notify_is_the_default_and_only_reports() {
        assert_eq!(UpdateConfig::default().policy, Policy::Notify);

        match decide(&UpdateConfig::default(), &behind(), 0, 4) {
            Decision::Available { what } => {
                assert_eq!(what.len(), 2, "{what:?}");
                assert!(what[0].contains("gamemode"));
                assert!(what[1].contains("engine"));
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    /// The one that matters most.
    #[test]
    fn it_refuses_to_update_while_anybody_is_playing() {
        match decide(&applying(), &behind(), 3, 4) {
            Decision::Deferred { why, .. } => assert!(why.contains("3 player(s)"), "{why}"),
            other => panic!("expected Deferred, got {other:?}"),
        }

        assert!(matches!(
            decide(&applying(), &behind(), 0, 4),
            Decision::Apply { .. }
        ));
    }

    #[test]
    fn only_when_empty_can_be_turned_off_deliberately() {
        let config = UpdateConfig {
            only_when_empty: false,
            ..applying()
        };
        assert!(matches!(
            decide(&config, &behind(), 30, 4),
            Decision::Apply { .. }
        ));
    }

    /// Pulling over somebody's uncommitted work is how an updater destroys it.
    #[test]
    fn it_refuses_to_pull_over_a_dirty_checkout() {
        let mut versions = behind();
        versions.git.as_mut().unwrap().dirty = true;

        match decide(&applying(), &versions, 0, 4) {
            Decision::Deferred { why, .. } => assert!(why.contains("uncommitted"), "{why}"),
            other => panic!("expected Deferred, got {other:?}"),
        }
    }

    #[test]
    fn a_dirty_checkout_does_not_block_an_engine_only_update() {
        let mut versions = behind();
        versions.git.as_mut().unwrap().dirty = true;

        let config = UpdateConfig {
            update_gamemode: false,
            update_engine: true,
            ..applying()
        };

        assert!(matches!(
            decide(&config, &versions, 0, 4),
            Decision::Apply { .. }
        ));
    }

    #[test]
    fn nothing_available_means_nothing_to_do() {
        let versions = Versions {
            git: Some(GitVersion {
                head: "aaaaaaaaaaaa".to_owned(),
                remote_head: Some("aaaaaaaaaaaa".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(decide(&applying(), &versions, 0, 4), Decision::UpToDate);
    }

    #[test]
    fn an_equal_window_means_any_time() {
        let config = UpdateConfig::default();
        for hour in 0..24 {
            assert!(in_window(&config, hour), "hour {hour}");
        }
    }

    #[test]
    fn a_window_that_wraps_midnight_works() {
        // 22:00 to 04:00, when a roleplay server is actually empty.
        let config = UpdateConfig {
            window_start_hour: 22,
            window_end_hour: 4,
            ..Default::default()
        };

        assert!(in_window(&config, 23));
        assert!(in_window(&config, 0));
        assert!(in_window(&config, 3));
        assert!(!in_window(&config, 4));
        assert!(!in_window(&config, 12));
        assert!(!in_window(&config, 21));
    }

    #[test]
    fn a_daytime_window_does_not_wrap() {
        let config = UpdateConfig {
            window_start_hour: 4,
            window_end_hour: 6,
            ..Default::default()
        };

        assert!(in_window(&config, 4));
        assert!(in_window(&config, 5));
        assert!(!in_window(&config, 6));
        assert!(!in_window(&config, 23));
    }

    #[test]
    fn outside_the_window_it_defers_and_says_so() {
        let config = UpdateConfig {
            window_start_hour: 4,
            window_end_hour: 5,
            ..applying()
        };

        match decide(&config, &behind(), 0, 12) {
            Decision::Deferred { why, .. } => assert!(why.contains("maintenance window"), "{why}"),
            other => panic!("expected Deferred, got {other:?}"),
        }
    }

    #[test]
    fn the_check_interval_has_a_floor() {
        let config = UpdateConfig {
            check_interval_minutes: 0,
            ..Default::default()
        };
        assert_eq!(interval(&config), Duration::from_secs(5 * 60));
    }

    #[tokio::test]
    async fn an_engine_update_without_steamcmd_fails_by_name_rather_than_silently() {
        let config = UpdateConfig {
            update_gamemode: false,
            update_engine: true,
            ..applying()
        };

        let applied = apply(&config, std::path::Path::new("/tmp")).await;
        assert!(!applied.ok);
        assert!(applied.steps[0].detail.contains("steamcmd"));
    }
}

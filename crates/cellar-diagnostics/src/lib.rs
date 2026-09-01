//! Every preflight check Cellar knows how to run, in one place.
//!
//! These used to live inside `cellar doctor` and print as they went, which made
//! them unreachable from anything but the CLI. `cellar-server` cannot depend on
//! `cellar-cli`, and a second copy of a check is a second copy that drifts, so
//! they moved here instead: the crate sits above `cellar-runtime` and
//! `cellar-store` because the checks genuinely span both.

use std::path::PathBuf;

use cellar_core::config::{Config, Instance};
use serde::Serialize;

/// What a check decided.
///
/// `Note` is not a third severity, it is the absence of a verdict: a fact worth
/// printing that Cellar cannot judge, such as a port being in use when the
/// server this profile describes is probably the thing using it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Fail,
    Note,
}

/// One question asked and answered.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// What was checked, in config-key terms where there is one.
    pub label: String,
    pub outcome: Outcome,
    pub detail: String,
    /// The instance this is about, when it is about one rather than the host.
    pub instance: Option<String>,
}

/// The result of a whole run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    /// How many checks failed. Zero is the only good answer.
    pub fn problems(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.outcome == Outcome::Fail)
            .count()
    }

    fn push(&mut self, instance: Option<&str>, outcome: Outcome, label: &str, detail: String) {
        self.checks.push(Check {
            label: label.to_owned(),
            outcome,
            detail,
            instance: instance.map(str::to_owned),
        });
    }

    fn check(&mut self, instance: Option<&str>, ok: bool, label: &str, detail: String) {
        self.push(
            instance,
            if ok { Outcome::Ok } else { Outcome::Fail },
            label,
            detail,
        );
    }

    fn note(&mut self, instance: Option<&str>, label: &str, detail: String) {
        self.push(instance, Outcome::Note, label, detail);
    }
}

/// Run everything, in the order a human wants to read it.
///
/// `owned_binds` are addresses the caller already holds. The dashboard runs
/// these same checks from inside a Cellar that is by definition bound to its
/// own web address, and reporting that as a conflict would make the screen
/// permanently red about the process rendering it.
pub async fn run(config: &Config, owned_binds: &[String]) -> Report {
    let mut report = Report::default();

    let instances = config.instances();
    for instance in &instances {
        if !instance.enabled {
            report.note(
                Some(instance.id.as_str()),
                "instance",
                "declared but not enabled".to_owned(),
            );
            continue;
        }
        check_one_server(instance, &mut report);
    }

    database(config, &mut report).await;
    binds(config, owned_binds, &mut report).await;
    ports(&instances, &mut report);
    disks(config, &instances, &mut report);
    steam_app(config, &mut report);

    for instance in instances.iter().filter(|instance| instance.enabled) {
        let log = instance.server.engine_log_file();
        report.note(
            Some(instance.id.as_str()),
            "log file",
            format!(
                "{} ({})",
                log.display(),
                if log.exists() {
                    "present"
                } else {
                    "not yet written"
                }
            ),
        );
    }

    report
}

async fn database(config: &Config, report: &mut Report) {
    if !config.database.enabled {
        return;
    }
    match config.database.url.as_ref() {
        Some(url) => match cellar_store::connect(url.expose(), 1).await {
            Ok(pool) => {
                let pending = cellar_store::MIGRATOR.iter().count();
                report.check(
                    None,
                    cellar_store::ping(&pool).await.is_ok(),
                    "database",
                    format!("reachable, {pending} migration(s) known"),
                );
            }
            Err(why) => report.check(None, false, "database", why.to_string()),
        },
        None => report.check(
            None,
            false,
            "database",
            "CELLAR_DATABASE_URL is not set".to_owned(),
        ),
    }
}

async fn binds(config: &Config, owned: &[String], report: &mut Report) {
    for (label, bind) in [
        ("web.bind", config.web.enabled.then_some(&config.web.bind)),
        (
            "bridge.bind",
            config.bridge.enabled.then_some(&config.bridge.bind),
        ),
    ] {
        let Some(bind) = bind else { continue };
        if owned.iter().any(|held| held == bind) {
            report.check(None, true, label, format!("{bind} is bound by this Cellar"));
            continue;
        }
        match address_is_free(bind).await {
            Free::Yes => report.check(None, true, label, format!("{bind} is free")),
            Free::HeldByCellar => report.check(
                None,
                false,
                label,
                format!(
                    "another Cellar is already bound to {bind}. Two Cellar processes against one \
                     profile run two backup loops that prune each other's dumps, two MariaDB \
                     instances against one data directory, and either may self-update the binary \
                     the other is executing."
                ),
            ),
            Free::HeldBySomethingElse(why) => report.check(
                None,
                false,
                label,
                format!("{bind} cannot be bound: {why}. Something else holds it."),
            ),
        }
    }
}

fn ports(instances: &[Instance], report: &mut Report) {
    for instance in instances.iter().filter(|instance| instance.enabled) {
        for (label, port) in [
            ("server.port", instance.server.port),
            ("server.query_port", instance.server.query_port),
        ] {
            if let Err(why) = std::net::UdpSocket::bind(("0.0.0.0", port)) {
                // A note, not a failure. The overwhelmingly common cause is the
                // server this profile describes already running, and nothing
                // here can tell that apart from a genuine conflict.
                report.note(
                    Some(instance.id.as_str()),
                    label,
                    format!(
                        "udp/{port} is in use ({why}). Expected if the server is already running; \
                         a conflict if it is not."
                    ),
                );
            } else {
                report.check(
                    Some(instance.id.as_str()),
                    true,
                    label,
                    format!("udp/{port} is free"),
                );
            }
        }
    }
}

fn disks(config: &Config, instances: &[Instance], report: &mut Report) {
    // Deduped by mount point, since instances sharing an install tree would
    // otherwise report the same filesystem several times.
    let mut disks: Vec<(Option<String>, String, PathBuf)> = Vec::new();
    for instance in instances.iter().filter(|instance| instance.enabled) {
        disks.push((
            Some(instance.id.to_string()),
            "disk, install".to_owned(),
            instance.server.executable.clone(),
        ));
        if let Some(dir) = &instance.server.data_dir {
            disks.push((
                Some(instance.id.to_string()),
                "disk, data".to_owned(),
                dir.clone(),
            ));
        }
    }
    if let Some(dir) = &config.backup.directory {
        disks.push((None, "disk, backups".to_owned(), dir.clone()));
    }

    let mut seen_mounts: Vec<PathBuf> = Vec::new();
    for (instance, label, path) in disks {
        let Some((free, mount)) = cellar_runtime::metrics::disk_free(&path) else {
            continue;
        };
        if seen_mounts.contains(&mount) {
            continue;
        }
        seen_mounts.push(mount.clone());
        // The dedicated server install alone is 4.9GB and a game update writes
        // before it deletes, so a couple of gigabytes is the point at which the
        // next update fails halfway rather than refusing.
        const FLOOR: u64 = 2 * 1024 * 1024 * 1024;
        report.check(
            instance.as_deref(),
            free >= FLOOR,
            &label,
            format!(
                "{:.1} GB free on {}{}",
                free as f64 / 1e9,
                mount.display(),
                if free >= FLOOR {
                    ""
                } else {
                    ", which is not enough headroom for a game update"
                }
            ),
        );
    }
}

/// Whether the configured map is one this gamemode has.
///
/// A map is a package ident, `org.name`, and it is the optional second
/// positional argument to `+game`; there is no `+map` switch. Shape alone was
/// all this could check, and a well-shaped ident for a map that does not exist
/// is a server that starts, fails to resolve the package and never becomes
/// ready, which is indistinguishable from a slow start.
fn map(instance: &Instance, report: &mut Report) {
    let id = Some(instance.id.as_str());
    let Some(map) = instance.server.map.as_deref() else {
        return;
    };

    let well_formed = map.split('.').count() == 2
        && map
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.');
    if !well_formed {
        report.check(
            id,
            false,
            "server.map",
            format!("'{map}' is not a package ident. A map is org.name, lowercase."),
        );
        return;
    }

    // The profile is the narrower statement and wins. Falling back to the
    // project's own list means a local development instance gets the check for
    // free, without anybody writing a profile at all.
    let declared: Vec<String> = if !instance.profile.maps.is_empty() {
        instance.profile.maps.clone()
    } else {
        cellar_update::project::read(&instance.server.project)
            .ok()
            .flatten()
            .map(|project| project.maps)
            .unwrap_or_default()
    };

    if declared.is_empty() {
        // Nothing said which maps exist, so nothing can be concluded. A note
        // rather than a pass, because "ok" would claim a check that did not
        // happen.
        report.note(
            id,
            "server.map",
            format!(
                "{map} is well-formed. Nothing declares which maps this gamemode has, so it                  cannot be checked; add [profile] maps or a MapList to the project."
            ),
        );
        return;
    }

    report.check(
        id,
        declared.iter().any(|known| known == map),
        "server.map",
        if declared.iter().any(|known| known == map) {
            map.to_owned()
        } else {
            format!("'{map}' is not one of: {}", declared.join(", "))
        },
    );
}

/// What the `.sbproj` says, for an instance that has one.
///
/// The player ceiling in particular: `+maxplayers` is not a convar and not a
/// launch switch, the old `entrypoint.sh` passed it for years and it was inert,
/// and `Metadata.MaxPlayers` is the only place the real number exists.
fn project(instance: &Instance, report: &mut Report) {
    let id = Some(instance.id.as_str());
    let path = &instance.server.project;

    match cellar_update::project::read(path) {
        Ok(Some(project)) => {
            let mut said = Vec::new();
            if let Some(ident) = project.package_ident() {
                said.push(format!("package {ident}"));
            }
            match project.max_players {
                Some(ceiling) => said.push(format!("{ceiling} players")),
                None => said.push(
                    "no MaxPlayers, so the ceiling is the engine's default and Cellar cannot                      report it"
                        .to_owned(),
                ),
            }
            if !project.packages.is_empty() {
                said.push(format!("{} package reference(s)", project.packages.len()));
            }
            report.note(id, "server.project metadata", said.join(", "));
        }
        Ok(None) => {}
        // Malformed rather than absent. Worth failing: the engine reads this
        // file too, and it will not start on one it cannot parse.
        Err(why) => report.check(id, false, "server.project metadata", why),
    }
}

/// Assertions the gamemode's own profile asked for.
///
/// This used to be one hardcoded check that grepped AppleJackRP's
/// `Code/Characters/CharacterDirector.cs` for two identifiers. It is now
/// whatever `[[profile.check]]` declares, so a gamemode Cellar has never heard
/// of gets the same treatment, and AppleJackRP's check lives in AppleJackRP's
/// profile where somebody who changes that file will find it.
fn check_profile(instance: &Instance, report: &mut Report) {
    // Relative to the project directory, which is where the check it replaced
    // looked. A published-package instance has no source tree to assert about,
    // so its checks are skipped rather than failed.
    let Some(root) = instance.server.project.parent() else {
        return;
    };
    if instance.server.project.as_os_str().is_empty() {
        return;
    }

    for declared in &instance.profile.checks {
        let path = root.join(&declared.file);
        let text = std::fs::read_to_string(&path);
        let passed = text
            .as_ref()
            .map(|body| declared.contains.iter().all(|needle| body.contains(needle)))
            .unwrap_or(false);

        report.check(
            Some(instance.id.as_str()),
            passed,
            &format!("gamemode: {}", declared.name),
            if passed {
                format!("{}", path.display())
            } else if text.is_err() {
                format!("{} is unreadable. {}", path.display(), declared.reason)
            } else {
                format!(
                    "{} does not contain {}. {}",
                    path.display(),
                    declared
                        .contains
                        .iter()
                        .map(|needle| format!("'{needle}'"))
                        .collect::<Vec<_>>()
                        .join(" and "),
                    declared.reason
                )
            },
        );
    }
}

/// The checks that are about one supervised server rather than about the host.
fn check_one_server(instance: &Instance, report: &mut Report) {
    let id = Some(instance.id.as_str());
    let server = &instance.server;
    let executable = &server.executable;
    report.check(
        id,
        executable.exists(),
        "server.executable",
        format!("{}", executable.display()),
    );

    if let Some(game) = server
        .game
        .as_deref()
        .filter(|game| !game.trim().is_empty())
    {
        report.check(id, true, "server.game", game.to_owned());
    } else {
        let project = &server.project;
        report.check(
            id,
            project.exists(),
            "server.project",
            format!("{}", project.display()),
        );

        // StartGame enumerates this and throws DirectoryNotFoundException when
        // it is absent, so the server exits to a bare console with no gamemode
        // loaded and on_failure retries it into the same wall. Git does not
        // keep empty directories, which is how a checkout loses it.
        if let Some(libraries) = project.parent().map(|dir| dir.join("Libraries")) {
            let present = libraries.is_dir();
            report.check(
                id,
                present,
                "project Libraries",
                if present {
                    format!("{}", libraries.display())
                } else {
                    format!(
                        "{} is missing, so the server will start and exit without loading the \
                         gamemode",
                        libraries.display()
                    )
                },
            );
        }
    }

    map(instance, report);
    project(instance, report);
    check_profile(instance, report);

    if let Some(dir) = &server.data_dir {
        match server.data_dir_mode_mismatch() {
            Some(why) => report.check(id, false, "server.data_dir mode", why),
            None => report.check(
                id,
                true,
                "server.data_dir mode",
                format!("{}", dir.display()),
            ),
        }
    }

    if server.launcher == cellar_core::Launcher::Wine {
        let wine = which("wine");
        report.check(
            id,
            wine.is_some(),
            "wine",
            wine.unwrap_or_else(|| "not on PATH; the Windows-only server cannot start".to_owned()),
        );

        if let Some(prefix) = &server.wine_prefix {
            // A prefix is not just a directory: the server needs the Windows
            // .NET 10 runtime inside it, and wine happily creates an empty one
            // on first use, so a missing runtime looks like a working prefix
            // right up until the server refuses to start.
            let runtime = prefix.join("drive_c/Program Files/dotnet/dotnet.exe");
            report.check(
                id,
                runtime.exists(),
                "server.wine_prefix",
                if runtime.exists() {
                    format!("{} has a .NET runtime", prefix.display())
                } else {
                    format!(
                        "{} has no dotnet.exe. The server needs the Windows .NET 10 runtime in \
                         its own prefix; see docs/INSTALLATION.md.",
                        prefix.display()
                    )
                },
            );
        }
    }

    if instance.bridge.enabled {
        report.check(
            id,
            server.data_dir.is_some(),
            "server.data_dir",
            match &server.data_dir {
                Some(dir) => format!("{}", dir.display()),
                None => "unset, so hosting.json cannot be written and the bridge will be unused"
                    .to_owned(),
            },
        );
    }
}

/// Whether an address Cellar wants can be bound, and if not, by what.
#[derive(Debug)]
pub enum Free {
    Yes,
    HeldByCellar,
    HeldBySomethingElse(String),
}

/// Try to bind, and when that fails, ask whoever holds it whether it is Cellar.
pub async fn address_is_free(bind: &str) -> Free {
    let Err(why) = std::net::TcpListener::bind(bind) else {
        return Free::Yes;
    };

    // Asking rather than assuming. "Address in use" says nothing about who, and
    // another Cellar is the case with real consequences: two backup loops
    // pruning each other, two MariaDB instances on one data directory.
    let probe = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build();
    if let Ok(client) = probe
        && let Ok(response) = client.get(format!("http://{bind}/healthz")).send().await
        && response.headers().contains_key(cellar_core::HEALTH_HEADER)
    {
        return Free::HeldByCellar;
    }

    Free::HeldBySomethingElse(why.to_string())
}

/// Whether the Steam install is the dedicated server rather than the client.
///
/// `read_installed_build` looks for `appmanifest_1892930.acf`, so an install of
/// 590830 makes version reporting silently impossible.
fn steam_app(config: &Config, report: &mut Report) {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = &config.update.steam_dir {
        roots.push(dir.clone());
    }
    if let Some(parent) = config
        .primary_server()
        .as_ref()
        .and_then(|server| server.executable.parent())
    {
        roots.push(parent.to_path_buf());
    }

    let mut apps = Vec::new();
    for root in &roots {
        apps.extend(cellar_update::version::installed_apps(root));
    }
    apps.dedup_by(|a, b| a.app_id == b.app_id);

    if apps.is_empty() {
        // A container image or a hand-copied tree has no manifest and still
        // runs, so this is not a failure.
        report.note(
            None,
            "steam app",
            "no appmanifest_*.acf found, so the installed build cannot be reported".to_owned(),
        );
        return;
    }

    let dedicated = cellar_update::version::SBOX_DEDICATED_APP_ID;
    if let Some(server) = apps.iter().find(|app| app.app_id == dedicated) {
        report.check(
            None,
            true,
            "steam app",
            format!("{} is app {dedicated}", server.name),
        );
        return;
    }

    let found = apps
        .iter()
        .map(|app| format!("{} ({})", app.app_id, app.name))
        .collect::<Vec<_>>()
        .join(", ");

    let client = cellar_update::version::SBOX_CLIENT_APP_ID;
    let detail = if apps.iter().any(|app| app.app_id == client) {
        format!(
            "this install is app {client}, the paid client and editor, not app {dedicated}, the \
             dedicated server. It carries sbox-dev.exe, editor/ and samples/, and no \
             appmanifest_{dedicated}.acf for version reporting to read. Install the server with: \
             steamcmd +login anonymous +app_update {dedicated} validate"
        )
    } else {
        format!("found {found}, but not app {dedicated}, the s&box dedicated server")
    };
    report.check(None, false, "steam app", detail);
}

/// The first executable of this name on `PATH`.
fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|found| found.display().to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_report_counts_only_failures_as_problems() {
        let mut report = Report::default();
        report.check(None, true, "a", "fine".to_owned());
        report.check(Some("dev"), false, "b", "broken".to_owned());
        report.note(None, "c", "unknowable".to_owned());

        assert_eq!(report.problems(), 1);
        assert_eq!(report.checks.len(), 3);
        assert_eq!(report.checks[1].instance.as_deref(), Some("dev"));
    }

    /// A well-shaped ident for a map that does not exist is the failure here.
    ///
    /// It starts, fails to resolve the package and never becomes ready, which
    /// is indistinguishable from a slow start. Shape alone could not catch it.
    #[tokio::test]
    async fn a_map_the_gamemode_does_not_have_is_refused_by_name() {
        let config: Config = toml::from_str(
            r#"
            [profile]
            name = "Facepunch Sandbox"
            map = ["facepunch.flatgrass"]

            [server]
            executable = "/nonexistent/sbox-server.exe"
            game = "facepunch.sandbox"
            map = "facepunch.flatgrasss"

            [web]
            enabled = false
            [bridge]
            enabled = false
            [database]
            enabled = false
            "#,
        )
        .expect("the fixture parses");

        let report = run(&config, &[]).await;
        let map = report
            .checks
            .iter()
            .find(|check| check.label == "server.map")
            .expect("the map is checked");

        assert_eq!(map.outcome, Outcome::Fail);
        // Naming what does exist is the whole value: "invalid map" tells an
        // operator nothing they can act on.
        assert!(
            map.detail.contains("facepunch.flatgrass"),
            "the refusal must list the maps that exist: {}",
            map.detail
        );
    }

    #[tokio::test]
    async fn a_gamemode_that_declares_no_maps_checks_nothing() {
        let config: Config = toml::from_str(
            r#"
            [server]
            executable = "/nonexistent/sbox-server.exe"
            game = "someone.gamemode"
            map = "someone.somemap"

            [web]
            enabled = false
            [bridge]
            enabled = false
            [database]
            enabled = false
            "#,
        )
        .expect("the fixture parses");

        let report = run(&config, &[]).await;
        let map = report
            .checks
            .iter()
            .find(|check| check.label == "server.map")
            .expect("the map is still mentioned");

        // A note, never an ok. "ok" would claim a check that did not happen,
        // and a gamemode that has not declared its maps is the common case.
        assert_eq!(map.outcome, Outcome::Note);
    }

    #[tokio::test]
    async fn a_disabled_instance_is_noted_rather_than_checked() {
        let config: Config = toml::from_str(
            r#"
            [instances.dev]
            enabled = false

            [instances.dev.server]
            executable = "/nonexistent/sbox-server.exe"

            [web]
            enabled = false

            [bridge]
            enabled = false

            [database]
            enabled = false
            "#,
        )
        .expect("the fixture parses");

        let report = run(&config, &[]).await;
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.outcome == Outcome::Note
                    && check.detail.contains("not enabled"))
        );
        assert_eq!(report.problems(), 0);
    }
}

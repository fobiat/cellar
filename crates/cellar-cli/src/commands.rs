//! The one-shot subcommands.
//!
//! None of these starts a server. They read a config, ask a question, print an
//! answer and exit, which is what makes them safe to run against a live
//! deployment and useful in a script.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cellar_core::config::Config;

use crate::{DbAction, DocAction, MariadbAction, McpAction};

/// Run Cellar's MCP stdio server or act as a small stdio MCP client.
pub async fn mcp(action: McpAction) -> Result<()> {
    match action {
        McpAction::Serve { url } => {
            let api =
                cellar_mcp::CellarApi::from_env(url.as_deref()).map_err(anyhow::Error::msg)?;
            cellar_mcp::CellarMcpServer::new(std::sync::Arc::new(api))
                .serve_stdio()
                .await
                .map_err(anyhow::Error::msg)
        }
        McpAction::Tools { command, args } => {
            let tools = cellar_mcp::list_child_tools(&command, &args)
                .await
                .map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&tools)?);
            Ok(())
        }
        McpAction::Call {
            command,
            tool,
            input,
            args,
        } => {
            let arguments = input
                .as_deref()
                .map(serde_json::from_str::<serde_json::Value>)
                .transpose()
                .context("--input must be valid JSON")?
                .map(|value| {
                    value
                        .as_object()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("--input must be a JSON object"))
                })
                .transpose()?;
            let result = cellar_mcp::call_child_tool(&command, &args, &tool, arguments)
                .await
                .map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}

/// Print the resolved config, with every secret already redacted by its type.
pub fn show_config(path: &Path) -> Result<()> {
    let config = Config::load(path).with_context(|| format!("reading {}", path.display()))?;

    // `Secret` serialises as `***`, so this cannot leak by omission of a field.
    println!("{}", toml::to_string_pretty(&config)?);

    println!("# secrets come from the environment and print as ***:");
    for (variable, present) in [
        (
            "CELLAR_GSLT",
            config
                .instances()
                .iter()
                .any(|instance| instance.server.gslt.is_some()),
        ),
        ("CELLAR_DATABASE_URL", config.database.url.is_some()),
        (
            "CELLAR_BRIDGE_SECRET",
            config.bridge.shared_secret.is_some(),
        ),
        (
            "CELLAR_WEB_PASSWORD_HASH",
            config.web.password_hash.is_some(),
        ),
        (
            "CELLAR_DISCORD_WEBHOOK_URL",
            config.notify.discord_webhook.is_some(),
        ),
    ] {
        println!(
            "#   {variable}: {}",
            if present { "set" } else { "not set" }
        );
    }

    Ok(())
}

/// Check everything that would otherwise fail at startup, and say what is wrong.
pub async fn doctor(path: &Path) -> Result<()> {
    let config = Config::load(path).with_context(|| format!("reading {}", path.display()))?;
    let mut problems = 0;

    let mut check = |ok: bool, label: &str, detail: String| {
        if ok {
            println!("  ok    {label}: {detail}");
        } else {
            println!("  FAIL  {label}: {detail}");
            problems += 1;
        }
    };

    // Per instance, not per config. One label prefix per instance so a config
    // with several says which one it is complaining about, and none so a
    // single-server config reads exactly as it did.
    let instances = config.instances();
    let one = instances.len() == 1;
    for instance in &instances {
        let name = |field: &str| {
            if one {
                field.to_owned()
            } else {
                format!("{}: {field}", instance.id)
            }
        };
        if !instance.enabled {
            println!("  note  {}: declared but not enabled", instance.id);
            continue;
        }
        check_one_server(instance, &name, &mut check);
    }

    if config.database.enabled {
        match config.database.url.as_ref() {
            Some(url) => match cellar_store::connect(url.expose(), 1).await {
                Ok(pool) => {
                    let pending = cellar_store::MIGRATOR.iter().count();
                    check(
                        cellar_store::ping(&pool).await.is_ok(),
                        "database",
                        format!("reachable, {pending} migration(s) known"),
                    );
                }
                Err(why) => check(false, "database", why.to_string()),
            },
            None => check(
                false,
                "database",
                "CELLAR_DATABASE_URL is not set".to_owned(),
            ),
        }
    }

    for (label, bind) in [
        ("web.bind", config.web.enabled.then_some(&config.web.bind)),
        (
            "bridge.bind",
            config.bridge.enabled.then_some(&config.bridge.bind),
        ),
    ] {
        let Some(bind) = bind else { continue };
        match address_is_free(bind).await {
            Free::Yes => check(true, label, format!("{bind} is free")),
            Free::HeldByCellar => check(
                false,
                label,
                format!(
                    "another Cellar is already bound to {bind}. Two Cellar processes against one \
                     profile run two backup loops that prune each other's dumps, two MariaDB \
                     instances against one data directory, and either may self-update the binary \
                     the other is executing."
                ),
            ),
            Free::HeldBySomethingElse(why) => check(
                false,
                label,
                format!("{bind} cannot be bound: {why}. Something else holds it."),
            ),
        }
    }

    let mut ports: Vec<(String, u16)> = Vec::new();
    for instance in instances.iter().filter(|instance| instance.enabled) {
        let name = |field: &str| {
            if one {
                field.to_owned()
            } else {
                format!("{}: {field}", instance.id)
            }
        };
        ports.push((name("server.port"), instance.server.port));
        ports.push((name("server.query_port"), instance.server.query_port));
    }
    for (label, port) in ports {
        let label = label.as_str();
        if let Err(why) = std::net::UdpSocket::bind(("0.0.0.0", port)) {
            // A note, not a failure. The overwhelmingly common cause is the
            // server this profile describes already running, and doctor cannot
            // tell that apart from a genuine conflict.
            println!(
                "  note  {label}: udp/{port} is in use ({why}). Expected if the server is already \
                 running; a conflict if it is not."
            );
        } else {
            check(true, label, format!("udp/{port} is free"));
        }
    }

    // Deduped by mount point, since instances sharing an install tree would
    // otherwise report the same filesystem several times.
    let mut disks: Vec<(String, std::path::PathBuf)> = Vec::new();
    for instance in instances.iter().filter(|instance| instance.enabled) {
        disks.push((
            format!("disk, {} install", instance.id),
            instance.server.executable.clone(),
        ));
        if let Some(dir) = &instance.server.data_dir {
            disks.push((format!("disk, {} data", instance.id), dir.clone()));
        }
    }
    if let Some(dir) = &config.backup.directory {
        disks.push(("disk, backups".to_owned(), dir.clone()));
    }
    let mut seen_mounts: Vec<std::path::PathBuf> = Vec::new();
    for (label, path) in disks {
        let label = label.as_str();
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
        check(
            free >= FLOOR,
            label,
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

    steam_app_check(&config, &mut check);

    for instance in instances.iter().filter(|instance| instance.enabled) {
        let log = instance.server.engine_log_file();
        let label = if one {
            "log file".to_owned()
        } else {
            format!("{}: log file", instance.id)
        };
        println!(
            "  note  {label}: {} ({})",
            log.display(),
            if log.exists() {
                "present"
            } else {
                "not yet written"
            }
        );
    }

    if problems == 0 {
        println!("\nNothing to fix.");
        Ok(())
    } else {
        anyhow::bail!("{problems} problem(s) above")
    }
}

/// Assertions the gamemode's own profile asked for.
///
/// This used to be one hardcoded check that grepped AppleJackRP's
/// `Code/Characters/CharacterDirector.cs` for two identifiers. It is now
/// whatever `[[profile.check]]` declares, so a gamemode Cellar has never heard
/// of gets the same treatment, and AppleJackRP's check lives in AppleJackRP's
/// profile where somebody who changes that file will find it.
fn check_profile(
    instance: &cellar_core::config::Instance,
    name: &impl Fn(&str) -> String,
    check: &mut impl FnMut(bool, &str, String),
) {
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

        check(
            passed,
            &name(&format!("gamemode: {}", declared.name)),
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
///
/// Split out so a config with several instances runs them once per instance.
fn check_one_server(
    instance: &cellar_core::config::Instance,
    name: &impl Fn(&str) -> String,
    check: &mut impl FnMut(bool, &str, String),
) {
    let server = &instance.server;
    let bridge_enabled = instance.bridge.enabled;
    let executable = &server.executable;
    check(
        executable.exists(),
        &name("server.executable"),
        format!("{}", executable.display()),
    );

    if let Some(game) = server
        .game
        .as_deref()
        .filter(|game| !game.trim().is_empty())
    {
        check(true, &name("server.game"), game.to_owned());
    } else {
        let project = &server.project;
        check(
            project.exists(),
            &name("server.project"),
            format!("{}", project.display()),
        );

        // StartGame enumerates this and throws DirectoryNotFoundException when it
        // is absent, so the server exits to a bare console with no gamemode
        // loaded and on_failure retries it into the same wall. Git does not keep
        // empty directories, which is how a checkout loses it.
        if let Some(libraries) = project.parent().map(|dir| dir.join("Libraries")) {
            let present = libraries.is_dir();
            check(
                present,
                &name("project Libraries"),
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

    if let Some(map) = server.map.as_deref() {
        check(
            map.split('.').count() == 2
                && map
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'),
            &name("server.map"),
            map.to_owned(),
        );
    }

    check_profile(instance, name, check);

    if let Some(dir) = &server.data_dir {
        match server.data_dir_mode_mismatch() {
            Some(why) => check(false, &name("server.data_dir mode"), why),
            None => check(
                true,
                &name("server.data_dir mode"),
                format!("{}", dir.display()),
            ),
        }
    }

    if server.launcher == cellar_core::Launcher::Wine {
        let wine = which("wine");
        check(
            wine.is_some(),
            &name("wine"),
            wine.unwrap_or_else(|| "not on PATH; the Windows-only server cannot start".to_owned()),
        );

        if let Some(prefix) = &server.wine_prefix {
            // A prefix is not just a directory: the server needs the Windows
            // .NET 10 runtime inside it, and wine happily creates an empty one
            // on first use, so a missing runtime looks like a working prefix
            // right up until the server refuses to start.
            let runtime = prefix.join("drive_c/Program Files/dotnet/dotnet.exe");
            check(
                runtime.exists(),
                &name("server.wine_prefix"),
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

    if bridge_enabled {
        check(
            server.data_dir.is_some(),
            &name("server.data_dir"),
            match &server.data_dir {
                Some(dir) => format!("{}", dir.display()),
                None => "unset, so hosting.json cannot be written and the bridge will be unused"
                    .to_owned(),
            },
        );
    }
}

/// Whether an address Cellar wants can be bound, and if not, by what.
enum Free {
    Yes,
    HeldByCellar,
    HeldBySomethingElse(String),
}

async fn address_is_free(bind: &str) -> Free {
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
        && response
            .text()
            .await
            .is_ok_and(|body| body.contains("\"cellar\"") || body.contains("\"state\""))
    {
        return Free::HeldByCellar;
    }

    Free::HeldBySomethingElse(why.to_string())
}

/// Whether the Steam install is the dedicated server rather than the client.
///
/// `read_installed_build` looks for `appmanifest_1892930.acf`, so an install of
/// 590830 makes version reporting silently impossible, and until now doctor
/// passed it without comment.
fn steam_app_check(config: &Config, check: &mut impl FnMut(bool, &str, String)) {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
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
        println!(
            "  note  steam app: no appmanifest_*.acf found, so the installed build cannot be \
             reported"
        );
        return;
    }

    let dedicated = cellar_update::version::SBOX_DEDICATED_APP_ID;
    if let Some(server) = apps.iter().find(|app| app.app_id == dedicated) {
        check(
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
    check(false, "steam app", detail);
}

/// Show installed and available versions.
pub async fn version(path: &Path, json: bool) -> Result<()> {
    let config = Config::load(path)?;
    let probe = probe_for(&config);
    let versions = cellar_update::version::probe(&probe).await;

    if json {
        println!("{}", serde_json::to_string_pretty(&versions)?);
        return Ok(());
    }

    match &versions.gamemode {
        Some(stamp) => println!(
            "gamemode   {} ({}, built {})",
            stamp.version, stamp.commit, stamp.build_date
        ),
        None => println!("gamemode   unknown (no BuildVersion.g.cs)"),
    }

    if let Some(git) = &versions.git {
        let dirty = if git.dirty {
            ", uncommitted changes"
        } else {
            ""
        };
        println!(
            "checkout   {} on {}{dirty}",
            git.short_head(),
            git.branch.as_deref().unwrap_or("a detached head")
        );

        match &git.remote_head {
            Some(remote) if git.is_behind() => {
                println!(
                    "remote     {} (differs, an update is available)",
                    &remote[..remote.len().min(8)]
                );
            }
            Some(_) => println!("remote     matches"),
            None => println!("remote     not checked"),
        }
    }

    match &versions.engine {
        Some(engine) => {
            println!("engine     build {}", engine.installed_build);
            match &engine.available_build {
                Some(available) if engine.is_behind() => {
                    println!("published  build {available} (an update is available)")
                }
                Some(_) => println!("published  matches"),
                None => println!("published  not checked"),
            }
        }
        None => println!("engine     unknown (no steam manifest at update.steam_dir)"),
    }

    for problem in &versions.problems {
        println!("note       {problem}");
    }

    Ok(())
}

/// Print the gamemode's changelog.
pub fn changelog(path: &Path, limit: usize, json: bool) -> Result<()> {
    let config = Config::load(path)?;
    let releases = cellar_update::read_changelog(&project_dir(&config), limit);

    if releases.is_empty() {
        anyhow::bail!("no CHANGELOG.md beside {}", project_dir(&config).display());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&releases)?);
        return Ok(());
    }

    for release in &releases {
        let date = release.date.as_deref().unwrap_or("unreleased");
        println!("\n\x1b[1m{}\x1b[0m  {date}", release.version);
        println!("{}", "-".repeat(release.version.len() + date.len() + 2));

        for section in &release.sections {
            println!("\n  {}", section.name);
            for item in &section.items {
                println!("    - {}", cellar_update::changelog::headline(item));
            }
        }
    }

    println!();
    Ok(())
}

/// Check for updates, and optionally take them.
///
/// Deliberately does not restart anything. `cellar run` owns the process and is
/// the only thing that may stop it; a second `cellar` invocation killing the
/// first one's child would be a surprising way to lose a server.
pub async fn update(path: &Path, check: bool, now: bool, force: bool) -> Result<()> {
    let config = Config::load(path)?;
    let probe = probe_for(&config);
    let versions = cellar_update::version::probe(&probe).await;

    let mut policy = config.update.clone();
    if check {
        policy.policy = cellar_core::config::UpdatePolicy::Notify;
    } else if now {
        policy.policy = cellar_core::config::UpdatePolicy::Apply;
        policy.window_start_hour = 0;
        policy.window_end_hour = 0;
        if force {
            policy.only_when_empty = false;
        }
    }

    // Nobody is connected as far as a one-shot invocation can tell, so it says
    // so rather than assuming zero and applying over a full server.
    let players = 0;
    let hour = chrono::Local::now()
        .format("%H")
        .to_string()
        .parse()
        .unwrap_or(0u8);

    match cellar_update::updater::decide(&policy, &versions, players, hour) {
        cellar_update::Decision::UpToDate => println!("Up to date. {}", versions.summary()),
        cellar_update::Decision::Available { what } => {
            println!("An update is available:");
            for line in what {
                println!("  {line}");
            }
            println!("\nRun `cellar update --now` to take it, or set update.policy = \"apply\".");
        }
        cellar_update::Decision::Deferred { what, why } => {
            println!("An update is available but was not applied ({why}):");
            for line in what {
                println!("  {line}");
            }
        }
        cellar_update::Decision::Apply { what } => {
            println!("Applying:");
            for line in &what {
                println!("  {line}");
            }

            let applied = cellar_update::updater::apply(&policy, &probe.project_dir).await;
            for step in &applied.steps {
                println!(
                    "  {} {}: {}",
                    if step.ok { "ok  " } else { "FAIL" },
                    step.name,
                    step.detail
                );
            }

            if !applied.ok {
                anyhow::bail!("the update did not complete");
            }
            println!("\nRestart the server for this to take effect.");
        }
    }

    Ok(())
}

pub async fn db(path: &Path, action: DbAction) -> Result<()> {
    let config = Config::load(path)?;
    let url = config
        .database
        .url
        .as_ref()
        .context("CELLAR_DATABASE_URL is not set")?;

    let pool = cellar_store::connect(url.expose(), config.database.max_connections).await?;

    match action {
        DbAction::Migrate => {
            cellar_store::migrate(&pool).await?;
            println!("Schema is current.");
        }
        DbAction::Status => {
            let tables = cellar_store::admin::tables(&pool).await?;
            println!("{:<26} {:>10} {:>12}", "table", "rows", "size");
            for table in tables {
                println!(
                    "{:<26} {:>10} {:>12}",
                    table.name,
                    table.rows,
                    cellar_runtime::metrics::format_bytes(table.bytes)
                );
            }
        }
        DbAction::Prune => {
            let removed =
                cellar_store::ops::prune_events(&pool, config.database.event_retention_days)
                    .await?;
            println!("Removed {removed} event(s).");
        }
        DbAction::Backup => {
            let output = cellar_mariadb::backup(url.expose(), &config.mariadb, &config.backup)
                .map_err(|why| anyhow::anyhow!(why))?;
            println!("Backup written to {}.", output.display());
        }
        DbAction::Backups => {
            let dumps = list_dumps(&config)?;
            if dumps.is_empty() {
                println!("No dumps yet. `cellar db backup` writes one.");
            }
            for dump in dumps {
                println!(
                    "{:<12} {}",
                    cellar_runtime::metrics::format_bytes(dump.bytes),
                    dump.path.display()
                );
            }
        }
        DbAction::Restore { dump, yes } => {
            let dump =
                match dump {
                    Some(path) => path,
                    None => list_dumps(&config)?
                        .into_iter()
                        .next()
                        .context(
                            "no dumps in the backup directory, and none named on the command line",
                        )?
                        .path,
                };

            refuse_if_a_cellar_is_running(&config).await?;

            if !yes {
                let database = url.expose().rsplit('/').next().unwrap_or("the database");
                println!(
                    "About to replace every table in '{database}' with the contents of {}.\n\
                     This cannot be undone. Take a backup first if you have not.",
                    dump.display()
                );
                print!("Type the database name to continue: ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut typed = String::new();
                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut typed)?;
                if typed.trim() != database {
                    anyhow::bail!("that is not the database name; nothing was changed");
                }
            }

            let restored = cellar_mariadb::restore(&dump, url.expose(), &config.mariadb)
                .map_err(|why| anyhow::anyhow!(why))?;
            println!(
                "Restored {} into '{}' from {}.",
                cellar_runtime::metrics::format_bytes(restored.bytes),
                restored.database,
                restored.from.display()
            );
        }
    }

    Ok(())
}

fn list_dumps(config: &Config) -> Result<Vec<cellar_mariadb::backup::Dump>> {
    let directory = backup_directory(config)
        .context("backup.directory is unset and there is no mariadb.data_dir to derive one from")?;
    match cellar_mariadb::backup::list(&directory) {
        Ok(dumps) => Ok(dumps),
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(why) => Err(why).with_context(|| format!("reading {}", directory.display())),
    }
}

/// Where dumps live, matching what `backup::create` decides.
fn backup_directory(config: &Config) -> Option<PathBuf> {
    config.backup.directory.clone().or_else(|| {
        config
            .mariadb
            .data_dir
            .as_ref()
            .map(|path| path.join("backups"))
    })
}

/// Refuse a restore while a Cellar is up, because it is supervising a server
/// that is writing to the database this is about to replace.
async fn refuse_if_a_cellar_is_running(config: &Config) -> Result<()> {
    if !config.web.enabled {
        return Ok(());
    }
    if !matches!(address_is_free(&config.web.bind).await, Free::HeldByCellar) {
        return Ok(());
    }

    anyhow::bail!(
        "a Cellar is answering on {}, so a supervised server is probably writing to this \
         database. Stop it first: the gamemode writes through the bridge continuously, and a \
         write landing mid-restore lands in a table that is about to be dropped.",
        config.web.bind
    )
}

/// Provision or report on the locally-hosted MariaDB.
///
/// Distinct from `db` above, which operates on whatever `database.url`
/// already points at, local or remote, and needs no `[mariadb]` section to
/// do it. This only makes sense when Cellar is hosting the instance itself.
pub async fn mariadb(path: &Path, action: MariadbAction) -> Result<()> {
    let config = Config::load(path)?;
    let mariadb = &config.mariadb;

    if !mariadb.managed {
        anyhow::bail!(
            "mariadb.managed is not set; Cellar is not configured to host its own database. \
             See the [mariadb] section in cellar.toml."
        );
    }

    match action {
        MariadbAction::Provision => {
            let client = reqwest_client()?;
            let url = cellar_mariadb::provision(mariadb, &client).await?;

            println!(
                "Provisioned mariadb {} on 127.0.0.1:{}.",
                mariadb.version, mariadb.port
            );
            println!("\nCELLAR_DATABASE_URL='{url}'");
            println!("\nSet this in your environment before running `cellar run`.");
        }
        MariadbAction::Status => {
            let installed = mariadb
                .install_dir
                .as_deref()
                .is_some_and(cellar_mariadb::release::already_installed);

            println!("version      {}", mariadb.version);
            println!("installed    {}", if installed { "yes" } else { "no" });
            println!("port         {}", mariadb.port);

            match mariadb
                .data_dir
                .as_deref()
                .and_then(cellar_mariadb::read_marker)
            {
                Some(marker) => println!(
                    "provisioned  yes (database `{}`, user `{}`)",
                    marker.database, marker.username
                ),
                None => println!("provisioned  no; run `cellar mariadb provision`"),
            }
        }
    }

    Ok(())
}

pub async fn doc(path: &Path, instance: Option<&str>, action: DocAction) -> Result<()> {
    let config = Config::load(path)?;
    let url = config
        .database
        .url
        .as_ref()
        .context("CELLAR_DATABASE_URL is not set")?;

    let pool = cellar_store::connect(url.expose(), 2).await?;
    // The named instance's scope. Documents are keyed on it, so reading the
    // primary's while `--instance` says otherwise lists the wrong server's
    // data under the right server's name.
    let scope = match instance {
        Some(_) => {
            resolve_instance(&config, instance)
                .context("no such instance in this config")?
                .scope
        }
        None => config.scope(),
    };

    match action {
        DocAction::Ls { prefix } => {
            let prefix = (!prefix.is_empty()).then_some(prefix);
            let documents =
                cellar_store::document::list(&pool, &scope, prefix.as_deref(), 500).await?;

            println!("{:<44} {:>6} {:>10}  updated", "key", "rev", "size");
            for document in documents {
                println!(
                    "{:<44} {:>6} {:>10}  {}",
                    document.key,
                    document.revision,
                    cellar_runtime::metrics::format_bytes(document.bytes),
                    document.updated_at.format("%Y-%m-%d %H:%M")
                );
            }
        }
        DocAction::Get { key } => {
            cellar_core::doc_key::check(&key).map_err(|e| anyhow::anyhow!("{e}"))?;
            let document = cellar_store::document::get(&pool, &scope, &key)
                .await?
                .with_context(|| format!("no document at '{key}'"))?;
            println!("{}", serde_json::to_string_pretty(&document.body)?);
        }
        DocAction::Put { key, file } => {
            cellar_core::doc_key::check(&key).map_err(|e| anyhow::anyhow!("{e}"))?;

            let text = if file == Path::new("-") {
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut String::new())
                    .map(|_| String::new())
                    .ok();
                std::io::read_to_string(std::io::stdin())?
            } else {
                std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?
            };

            let body: serde_json::Value =
                serde_json::from_str(&text).context("the file is not valid JSON")?;

            let outcome =
                cellar_store::document::put(&pool, &scope, &key, &body, Some("cellar-cli"), None)
                    .await?;
            println!(
                "Wrote '{key}' as revision {}{}.",
                outcome.revision,
                if outcome.created { " (new)" } else { "" }
            );
        }
        DocAction::History { key } => {
            cellar_core::doc_key::check(&key).map_err(|e| anyhow::anyhow!("{e}"))?;
            for revision in cellar_store::document::revisions(&pool, &scope, &key, 50).await? {
                println!(
                    "r{:<6} {}  {}",
                    revision.revision,
                    revision.written_at.format("%Y-%m-%d %H:%M:%S"),
                    revision.written_by.as_deref().unwrap_or("unknown")
                );
            }
        }
    }

    Ok(())
}

/// Read and write the running server's configuration.
///
/// These drive the running instance through its web API rather than through a
/// socket of their own. `cellar run` owns the console; a second process opening
/// its own channel to the same terminal is two writers on one file descriptor,
/// and the interleaving is exactly as bad as it sounds.
pub async fn settings(
    path: &Path,
    instance: Option<&str>,
    action: crate::SettingsAction,
) -> Result<()> {
    let config = Config::load(path)?;
    let client = LiveServer::connect(&config, instance).await?;

    match action {
        crate::SettingsAction::Dump {
            yaml,
            output,
            overrides,
            find,
        } => {
            let mut snapshot = client.capture(&find).await?;
            snapshot.hostname = config
                .primary_server()
                .map(|server| server.hostname.clone());
            snapshot.captured_at = Some(chrono::Utc::now().to_rfc3339());

            if overrides {
                snapshot = snapshot.overrides_only();
            }

            let text = if yaml {
                snapshot.to_yaml()
            } else {
                snapshot.to_toml()
            }
            .map_err(anyhow::Error::msg)?;

            match output {
                Some(file) => {
                    std::fs::write(&file, &text)
                        .with_context(|| format!("writing {}", file.display()))?;
                    println!(
                        "Wrote {} feature(s), {} setting(s) and {} convar(s) to {}.",
                        snapshot.features.len(),
                        snapshot.settings.len(),
                        snapshot.convars.len(),
                        file.display()
                    );
                }
                None => print!("{text}"),
            }
        }

        crate::SettingsAction::Diff { file } => {
            let changes = client.plan_from_file(&file).await?;
            print_changes(&changes);
        }

        crate::SettingsAction::Apply { file, dry_run } => {
            let changes = client.plan_from_file(&file).await?;

            if changes.is_empty() {
                println!("Already matches.");
                return Ok(());
            }

            print_changes(&changes);

            if dry_run {
                println!("\nDry run. Nothing was sent.");
                return Ok(());
            }

            println!();
            let mut applied = 0;
            for change in &changes {
                if let Some(why) = &change.refused {
                    println!("  skip  {}: {why}", change.id);
                    continue;
                }

                let reply = client.exec(&change.command).await?;
                // The gamemode refuses an unknown id or an out-of-bounds value
                // by name and without writing anything, so its own words are
                // the most useful thing to show.
                let refused = reply.iter().any(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower.contains("refus")
                        || lower.contains("not a valid")
                        || lower.contains("unknown")
                });

                if refused {
                    println!("  FAIL  {}: {}", change.id, reply.join(" "));
                } else {
                    applied += 1;
                    println!("  ok    {} -> {}", change.id, change.to);
                }
            }

            println!("\nApplied {applied} of {}.", changes.len());

            if changes.iter().any(|change| change.needs_restart) {
                println!("Some of these are read once at boot; restart for them to take effect.");
            }
        }

        crate::SettingsAction::Set { id, value } => {
            let snapshot = client.capture("").await?;

            // Which command depends on which catalogue the id is in, and asking
            // the server beats guessing from the id's shape.
            let command = if snapshot.feature(&id).is_some() {
                let enabled = matches!(
                    value.to_ascii_lowercase().as_str(),
                    "on" | "true" | "1" | "enabled"
                );
                client.catalogue()?.feature_command(&id, enabled)
            } else if snapshot.setting(&id).is_some() {
                client.catalogue()?.setting_command(&id, &value)
            } else {
                anyhow::bail!("this server has no feature or setting called '{id}'");
            };

            println!("> {command}");
            for line in client.exec(&command).await? {
                println!("  {line}");
            }
        }
    }

    Ok(())
}

pub async fn exec(
    path: &Path,
    instance: Option<&str>,
    command: Vec<String>,
    file: Option<std::path::PathBuf>,
    json: bool,
    keep_going: bool,
) -> Result<()> {
    let commands = match &file {
        Some(file) => {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()))?;
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned)
                .collect()
        }
        // Trailing args arrive already split, and the console takes one line.
        None => vec![command.join(" ").trim().to_owned()],
    };

    if commands.iter().all(String::is_empty) {
        anyhow::bail!("nothing to run: give a command, or a --file with one per line");
    }

    let config = Config::load(path)?;
    let client = LiveServer::connect(&config, instance).await?;

    let mut failed = 0usize;

    for command in &commands {
        match client.exec(command).await {
            Ok(reply) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "command": command, "reply": reply, "ok": true })
                    );
                } else {
                    // A file of commands needs to say which reply belongs to
                    // which; a single command's reply speaks for itself.
                    if file.is_some() {
                        println!("> {command}");
                    }
                    for line in &reply {
                        println!("{line}");
                    }
                }
            }
            Err(why) => {
                failed += 1;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "command": command,
                            "error": why.to_string(),
                            "ok": false,
                        })
                    );
                } else {
                    eprintln!("{command}: {why}");
                }

                if !keep_going {
                    break;
                }
            }
        }
    }

    if failed > 0 {
        anyhow::bail!("{failed} of {} command(s) failed", commands.len());
    }

    Ok(())
}

fn print_changes(changes: &[cellar_core::convar::Change]) {
    if changes.is_empty() {
        println!("No changes.");
        return;
    }

    for change in changes {
        let note = match (&change.refused, change.needs_restart) {
            (Some(why), _) => format!("  ({why})"),
            (None, true) => "  (needs a restart)".to_owned(),
            (None, false) => String::new(),
        };
        println!("  {:<34} {} -> {}{note}", change.id, change.from, change.to);
    }
}

/// The instance `--instance` names, or the primary when it names nothing.
///
/// Returns `None` for an id this config does not declare, so a caller can say
/// so rather than fall back to the primary. A silent fallback here is the same
/// mistake the HTTP `Target` extractor refuses to make.
fn resolve_instance(
    config: &Config,
    instance: Option<&str>,
) -> Option<cellar_core::config::Instance> {
    match instance.filter(|value| !value.trim().is_empty()) {
        Some(wanted) => config
            .instances()
            .into_iter()
            .find(|candidate| candidate.id.as_str() == wanted.trim()),
        None => config.primary(),
    }
}

/// The running `cellar run`, reached through its web API.
struct LiveServer {
    client: reqwest::Client,
    base: String,
    /// Which supervised server these calls are about. `None` is the primary.
    instance: Option<String>,
    /// The gamemode's convar prefix, from `[profile]`. Absent means this
    /// gamemode has no settings catalogue Cellar knows how to ask for.
    convar_prefix: Option<String>,
}

impl LiveServer {
    async fn connect(config: &Config, instance: Option<&str>) -> Result<Self> {
        // Refused here rather than by the server, so a typo costs one message
        // instead of a connection, a login and a 404.
        if let Some(wanted) = instance.filter(|value| !value.trim().is_empty())
            && resolve_instance(config, Some(wanted)).is_none()
        {
            anyhow::bail!(
                "no instance '{}' in this config. It declares: {}",
                wanted.trim(),
                config
                    .instances()
                    .iter()
                    .map(|candidate| candidate.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        if !config.web.enabled {
            anyhow::bail!(
                "these commands talk to a running `cellar run` through its web API, and \
                 web.enabled is false in this config"
            );
        }

        let base = format!("http://{}", config.web.bind.replace("0.0.0.0", "127.0.0.1"));

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .cookie_store(true)
            .build()?;

        // A password is only needed when the web UI has one, which the config
        // layer only permits off loopback.
        if config.web.password_hash.is_some() {
            let password = std::env::var("CELLAR_WEB_PASSWORD").context(
                "this server's web UI has a password; set CELLAR_WEB_PASSWORD to use these commands",
            )?;

            let response = client
                .post(format!("{base}/api/login"))
                .json(&serde_json::json!({ "password": password }))
                .send()
                .await
                .with_context(|| format!("reaching {base}"))?;

            if !response.status().is_success() {
                anyhow::bail!("CELLAR_WEB_PASSWORD was not accepted");
            }
        }

        let server = Self {
            client,
            base,
            instance: instance
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().to_owned()),
            // The named instance's profile, not the primary's. Two instances
            // may run different gamemodes, and reading the wrong prefix sends
            // one gamemode's catalogue command to the other's console.
            convar_prefix: resolve_instance(config, instance)
                .and_then(|instance| instance.profile.convar_prefix.clone()),
        };

        server
            .client
            .get(server.url("/api/status")?)
            .send()
            .await
            .with_context(|| {
                format!(
                    "no running Cellar at {}. Start one with `cellar run`.",
                    server.base
                )
            })?
            .error_for_status()
            .context("the running Cellar refused the request")?;

        Ok(server)
    }

    /// Build a route URL, adding `?instance=` when one was named.
    ///
    /// A query pair rather than string concatenation, so an id carrying a `&`
    /// becomes a 404 from Cellar rather than a request that means something
    /// else than it reads.
    fn url(&self, path: &str) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.base))
            .with_context(|| format!("building a URL for {path}"))?;
        if let Some(id) = &self.instance {
            url.query_pairs_mut().append_pair("instance", id);
        }
        Ok(url)
    }

    async fn exec(&self, command: &str) -> Result<Vec<String>> {
        let response = self
            .client
            .post(self.url("/api/exec")?)
            .json(&serde_json::json!({ "command": command }))
            .send()
            .await?;

        let body: serde_json::Value = response.json().await?;

        if let Some(error) = body.get("error").and_then(|e| e.as_str()) {
            anyhow::bail!("{error}");
        }

        Ok(body
            .get("reply")
            .and_then(|reply| reply.as_array())
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The four catalogue commands for this gamemode, or why there are none.
    fn catalogue(&self) -> Result<cellar_core::convar::Catalogue<'_>> {
        match self.convar_prefix.as_deref() {
            Some(prefix) => Ok(cellar_core::convar::Catalogue::new(prefix)),
            None => anyhow::bail!(
                "this gamemode's profile declares no convar_prefix, so Cellar does not know what \
                 to ask it for. Add one to [profile] in the config."
            ),
        }
    }

    /// Ask the server for everything it is set to.
    async fn capture(&self, find: &str) -> Result<cellar_core::convar::Snapshot> {
        use cellar_core::convar;

        let catalogue = self.catalogue()?;
        let features = convar::parse_features(&self.exec(&catalogue.list_features()).await?);
        let settings = convar::parse_settings(&self.exec(&catalogue.list_settings()).await?);

        let convars = if find.is_empty() {
            Vec::new()
        } else {
            convar::parse_convars(&self.exec(&format!("find {find}")).await?)
        };

        Ok(convar::Snapshot {
            captured_at: None,
            hostname: None,
            features,
            settings,
            convars,
        })
    }

    async fn plan_from_file(&self, file: &Path) -> Result<Vec<cellar_core::convar::Change>> {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;

        let desired = cellar_core::convar::Snapshot::parse(&text).map_err(anyhow::Error::msg)?;
        let current = self.capture("").await?;

        Ok(cellar_core::convar::plan(
            &self.catalogue()?,
            &current,
            &desired,
        ))
    }
}

/// Update Cellar itself.
///
/// Distinct from `cellar update`, which updates the *game*. This one replaces
/// the running binary, and refuses to install anything whose SHA-256 does not
/// match the checksum published beside it.
pub async fn self_update(check_only: bool) -> Result<()> {
    use cellar_update::selfupdate;

    let running = std::env::current_exe().context("finding this binary")?;
    // Sweep a `.old` left by a previous update. On Windows it is only
    // deletable once the process that was running it has exited, which is why
    // this happens here rather than at the end of the update.
    selfupdate::sweep(&running);

    let current = env!("CARGO_PKG_VERSION");
    let target = selfupdate::current_target();

    let client = reqwest_client()?;
    let response = github(client.get(selfupdate::DEFAULT_RELEASES_URL))
        .send()
        .await
        .context("reaching the release API")?;

    // 404 means either "no releases" or "this repository is not visible to
    // you", and those need different actions. Saying which one is the whole
    // difference between a useful message and a confusing one.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "no release visible at {}.\n\
             If the repository is private, set CELLAR_GITHUB_TOKEN to a token with `repo` scope \
             (`gh auth token` prints one). Otherwise there is no release published yet.",
            selfupdate::DEFAULT_RELEASES_URL
        );
    }

    let json: serde_json::Value = response
        .error_for_status()
        .context("the release API refused the request")?
        .json()
        .await
        .context("reading the release document")?;

    let Some(release) = selfupdate::parse_release(&json) else {
        anyhow::bail!("the release API answered with something this build cannot read");
    };

    if !selfupdate::is_newer(current, &release.tag) {
        println!("Cellar {current} is current.");
        return Ok(());
    }

    println!("Cellar {} is available (running {current}).", release.tag);
    if !release.notes.trim().is_empty() {
        println!();
        for line in release.notes.lines().take(20) {
            println!("  {line}");
        }
    }

    if check_only {
        println!("\nRun `cellar self-update` to install it.");
        return Ok(());
    }

    // The bare binary, not the archive: replacing one file needs no unpacking.
    let asset = release
        .binary_for(target)
        .with_context(|| format!("no bare binary asset for {target} in {}", release.tag))?;

    let checksum_asset = release
        .checksum_for(asset)
        .context("no published checksum; refusing to install an unverified binary")?;

    println!("\nDownloading {}", asset.name);
    let bytes = github(client.get(&asset.url))
        .header("Accept", "application/octet-stream")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let checksum = github(client.get(&checksum_asset.url))
        .header("Accept", "application/octet-stream")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    selfupdate::verify(&bytes, &checksum).map_err(anyhow::Error::new)?;
    println!("Checksum matches.");

    let retired = selfupdate::install(&running, &bytes).map_err(anyhow::Error::new)?;

    println!("Installed {} to {}.", release.tag, running.display());
    if retired.exists() {
        // Windows cannot delete the image it is executing. The next run sweeps it.
        println!(
            "The previous binary is at {}; it is removed on the next run.",
            retired.display()
        );
    }
    println!("\nRestart Cellar for the new version to take effect.");

    Ok(())
}

/// Add the headers GitHub wants, including a token when one is configured.
///
/// A private repository answers 404 to an anonymous caller, so a token is the
/// difference between self-update working and appearing to have no releases.
fn github(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let request = request
        .header("User-Agent", "cellar")
        .header("Accept", "application/vnd.github+json");

    match std::env::var("CELLAR_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .ok()
        .filter(|token| !token.trim().is_empty())
    {
        Some(token) => request.header("Authorization", format!("Bearer {token}")),
        None => request,
    }
}

fn reqwest_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("building an http client")
}

/// Hash a password so a plaintext one never has to be typed into a file.
pub fn hash_password() -> Result<()> {
    let password = rpassword::prompt_password("Operator password: ")?;
    let again = rpassword::prompt_password("Again: ")?;

    if password != again {
        anyhow::bail!("the two did not match");
    }
    if password.len() < 10 {
        anyhow::bail!(
            "use at least 10 characters; this guards a console with full engine privilege"
        );
    }

    let hash = cellar_server::session::hash_password(&password).map_err(anyhow::Error::msg)?;
    println!("\nCELLAR_WEB_PASSWORD_HASH='{hash}'");
    Ok(())
}

fn probe_for(config: &Config) -> cellar_update::Probe {
    cellar_update::Probe {
        project_dir: project_dir(config),
        steam_dir: config.update.steam_dir.clone(),
        steamcmd: config
            .update
            .steamcmd
            .clone()
            .or_else(|| which("steamcmd").map(std::path::PathBuf::from)),
        check_remote: config.update.check_remote,
    }
}

fn project_dir(config: &Config) -> std::path::PathBuf {
    config
        .primary_server()
        .as_ref()
        .and_then(|server| server.project.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Find an executable on PATH, without a dependency for it.
fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then(|| candidate.display().to_string())
    })
}

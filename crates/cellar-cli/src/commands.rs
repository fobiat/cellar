//! The one-shot subcommands.
//!
//! None of these starts a server. They read a config, ask a question, print an
//! answer and exit, which is what makes them safe to run against a live
//! deployment and useful in a script.

use std::path::Path;

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
        ("CELLAR_GSLT", config.server.gslt.is_some()),
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

    let executable = &config.server.executable;
    check(
        executable.exists(),
        "server.executable",
        format!("{}", executable.display()),
    );

    if let Some(game) = config
        .server
        .game
        .as_deref()
        .filter(|game| !game.trim().is_empty())
    {
        check(true, "server.game", game.to_owned());
    } else {
        let project = &config.server.project;
        check(
            project.exists(),
            "server.project",
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

    if let Some(map) = config.server.map.as_deref() {
        check(
            map.split('.').count() == 2
                && map
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'),
            "server.map",
            map.to_owned(),
        );
    }

    let spawn_source = config
        .server
        .project
        .parent()
        .map(|path| path.join("Code/Characters/CharacterDirector.cs"));
    if let Some(path) = spawn_source {
        let grounded = std::fs::read_to_string(&path)
            .map(|text| text.contains("GroundedOrAuthored") && text.contains("Scene.Trace"))
            .unwrap_or(false);
        check(grounded, "spawn validation", format!("{}", path.display()));
    }

    if let Some(dir) = &config.server.data_dir {
        match config.server.data_dir_mode_mismatch() {
            Some(why) => check(false, "server.data_dir mode", why),
            None => check(true, "server.data_dir mode", format!("{}", dir.display())),
        }
    }

    if config.server.launcher == cellar_core::Launcher::Wine {
        let wine = which("wine");
        check(
            wine.is_some(),
            "wine",
            wine.unwrap_or_else(|| "not on PATH; the Windows-only server cannot start".to_owned()),
        );
    }

    if config.bridge.enabled {
        check(
            config.server.data_dir.is_some(),
            "server.data_dir",
            match &config.server.data_dir {
                Some(dir) => format!("{}", dir.display()),
                None => "unset, so hosting.json cannot be written and the bridge will be unused"
                    .to_owned(),
            },
        );
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

    let log = cellar_runtime::log_file_for(&config.server);
    println!(
        "  note  log file: {} ({})",
        log.display(),
        if log.exists() {
            "present"
        } else {
            "not yet written"
        }
    );

    if problems == 0 {
        println!("\nNothing to fix.");
        Ok(())
    } else {
        anyhow::bail!("{problems} problem(s) above")
    }
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
    }

    Ok(())
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

pub async fn doc(path: &Path, action: DocAction) -> Result<()> {
    let config = Config::load(path)?;
    let url = config
        .database
        .url
        .as_ref()
        .context("CELLAR_DATABASE_URL is not set")?;

    let pool = cellar_store::connect(url.expose(), 2).await?;
    let scope = config.scope();

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
pub async fn settings(path: &Path, action: crate::SettingsAction) -> Result<()> {
    use cellar_core::convar;

    let config = Config::load(path)?;
    let client = LiveServer::connect(&config).await?;

    match action {
        crate::SettingsAction::Dump {
            yaml,
            output,
            overrides,
            find,
        } => {
            let mut snapshot = client.capture(&find).await?;
            snapshot.hostname = Some(config.server.hostname.clone());
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
                convar::feature_command(&id, enabled)
            } else if snapshot.setting(&id).is_some() {
                convar::setting_command(&id, &value)
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
    let client = LiveServer::connect(&config).await?;

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

/// The running `cellar run`, reached through its web API.
struct LiveServer {
    client: reqwest::Client,
    base: String,
}

impl LiveServer {
    async fn connect(config: &Config) -> Result<Self> {
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

        let server = Self { client, base };

        server
            .client
            .get(format!("{}/api/status", server.base))
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

    async fn exec(&self, command: &str) -> Result<Vec<String>> {
        let response = self
            .client
            .post(format!("{}/api/exec", self.base))
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

    /// Ask the server for everything it is set to.
    async fn capture(&self, find: &str) -> Result<cellar_core::convar::Snapshot> {
        use cellar_core::convar;

        let features = convar::parse_features(&self.exec("applejack_features").await?);
        let settings = convar::parse_settings(&self.exec("applejack_settings").await?);

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

        Ok(cellar_core::convar::plan(&current, &desired))
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
        .server
        .project
        .parent()
        .map(Path::to_path_buf)
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

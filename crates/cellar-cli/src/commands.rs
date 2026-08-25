//! The one-shot subcommands.
//!
//! None of these starts a server. They read a config, ask a question, print an
//! answer and exit, which is what makes them safe to run against a live
//! deployment and useful in a script.

use std::path::Path;

use anyhow::{Context, Result};
use cellar_core::config::Config;

use crate::{DbAction, DocAction};

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

    let project = &config.server.project;
    check(
        project.exists(),
        "server.project",
        format!("{}", project.display()),
    );

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
                    revision.written_by.as_deref().unwrap_or("—")
                );
            }
        }
    }

    Ok(())
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
    let json: serde_json::Value = client
        .get(selfupdate::DEFAULT_RELEASES_URL)
        .header("User-Agent", "cellar")
        .send()
        .await
        .context("reaching the release API")?
        .json()
        .await
        .context("reading the release document")?;

    let Some(release) = selfupdate::parse_release(&json) else {
        anyhow::bail!("no releases published yet");
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
    let bytes = client
        .get(&asset.url)
        .header("User-Agent", "cellar")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let checksum = client
        .get(&checksum_asset.url)
        .header("User-Agent", "cellar")
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

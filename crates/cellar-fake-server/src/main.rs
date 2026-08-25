//! A stand-in for `sbox-server.exe`.
//!
//! End-to-end testing of a supervisor for a Windows-only game engine otherwise
//! needs Steam, a GSLT, Wine and a real s&box build, which means it does not
//! happen in CI and barely happens locally. This emits the engine's actual log
//! formats, writes a status bar, reads commands from its console, and can be
//! told to crash, hang or flood.
//!
//! Every string it prints is copied from engine source, so a test asserting
//! against them is asserting against the real thing:
//!
//! - console: `hh:mm:ss ` + logger padded to 8 + ` ` + message (`GameLog.cs`)
//! - file:    `yyyy/MM/dd HH:mm:ss.ffff` TAB `[logger] message` TAB exception
//! - join:    `{name} [{steamid}] is connected` (`NetworkSystem.Handshake.cs`)
//! - leave:   `{name} [{steamid}] disconnected` (`NetworkSystem.Connections.cs`)
//!
//! Usage: `cellar-fake-server [--log-file PATH] [--crash-after N] [--hang]
//!         [--ignore-quit] [--players N] [--flood]`

use std::io::{BufRead, Write};
use std::path::PathBuf;

/// The feature catalogue, in the shape `applejack_features` prints.
const FEATURES: &[(&str, &str, &str)] = &[
    ("ui.menu.admin", "live", "Admin panel"),
    ("ui.menu.devmenu", "live", "Developer menu"),
    ("economy.manufacture", "core", "Manufacture"),
    ("world.daynight", "boot", "Day and night cycle"),
];

/// The setting catalogue: id, default, bounds, source.
const SETTINGS: &[(&str, &str, &str, &str)] = &[
    (
        "combat.respawn_wait_seconds",
        "30",
        "[0, 600]",
        "sh_config.lua Spawn Time",
    ),
    (
        "crime.arrest_seconds",
        "300",
        "[0, 3600]",
        "sh_config.lua Arrest Time",
    ),
    (
        "chat.talk_radius",
        "256",
        "[0, 4096]",
        "sh_config.lua Talk Radius",
    ),
];

struct Options {
    log_file: Option<PathBuf>,
    crash_after: Option<u64>,
    hang: bool,
    ignore_quit: bool,
    players: usize,
    flood: bool,
    hostname: String,
    max_players: u32,
}

fn main() -> std::process::ExitCode {
    let options = parse_args();

    let mut log = options.log_file.as_ref().and_then(|path| {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });

    emit(&mut log, "Bootstrap", "Starting dedicated server");
    emit(
        &mut log,
        "Bootstrap",
        &format!("hostname is {}", options.hostname),
    );

    if options.hang {
        // Never becomes ready. The readiness probe must stay 503 and the
        // graceful stop must escalate to a kill.
        emit(&mut log, "Bootstrap", "loading map");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    // The line AppleJackRP's NetworkBootstrap logs, and Cellar's default
    // readiness pattern.
    emit(&mut log, "Bootstrap", "Lobby created - session is joinable");

    for index in 0..options.players {
        let steam_id = 76561198000000000u64 + index as u64;
        emit(
            &mut log,
            "Network",
            &format!("Player{index} [{steam_id}] is connected"),
        );
    }

    if options.flood {
        for index in 0..50_000 {
            emit(&mut log, "Spam", &format!("line {index}"));
        }
    }

    status_bar(&options, options.players as u32, 0);

    let started = std::time::Instant::now();
    let stdin = std::io::stdin();
    let mut connected = options.players;

    // Feature and setting state, so a write is observable by the next read.
    // Without that, `settings apply` cannot be tested end to end.
    let mut features: Vec<String> = vec!["economy.manufacture".to_owned()];
    let mut settings: Vec<(String, String)> = Vec::new();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let command = line.trim();

        if let Some(after) = options.crash_after
            && started.elapsed().as_secs() >= after
        {
            emit(&mut log, "Engine", "unhandled exception, terminating");
            return std::process::ExitCode::from(1);
        }

        match command {
            "" => {}
            "quit" => {
                if options.ignore_quit {
                    emit(&mut log, "Engine", "ignoring quit, as instructed");
                    continue;
                }

                // The nine shutdown steps, in the order AppSystem.cs runs them.
                emit(&mut log, "Engine", "CloseGame");
                emit(&mut log, "Api", "flushing analytics");
                emit(&mut log, "ConVar", "SaveAll");
                emit(&mut log, "Network", "DedicatedServer.Shutdown, LogOff");
                return std::process::ExitCode::SUCCESS;
            }
            "status" => {
                emit(&mut log, "Network", "PLAYERS ----------");
                for index in 0..connected {
                    let steam_id = 76561198000000000u64 + index as u64;
                    emit(
                        &mut log,
                        "Network",
                        &format!("{index}\t{steam_id}\tActive\t\tPlayer{index}\t\t00:10:00"),
                    );
                }
            }
            "applejack_features" => {
                // The fixed-width shape FeatureDirector.cs prints.
                emit(&mut log, "Features", "[Features] 41 feature(s)");
                for (id, class, title) in FEATURES {
                    let on = features.iter().any(|held| held == id);
                    let default_on = *id == "economy.manufacture";
                    let state = format!(
                        "{} (default {})",
                        if on { "on" } else { "off" },
                        if default_on { "on" } else { "off" }
                    );
                    emit(
                        &mut log,
                        "Features",
                        &format!("{id:<28} {state:<36} {class:<5} {title}"),
                    );
                }
            }
            "applejack_settings" => {
                emit(
                    &mut log,
                    "Settings",
                    "[Settings] a value takes effect where its call site reads it",
                );
                for (id, default, bounds, source) in SETTINGS {
                    let value = settings
                        .iter()
                        .find(|(key, _)| key == id)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_else(|| (*default).to_owned());
                    emit(
                        &mut log,
                        "Settings",
                        &format!(
                            "{id:<34} {value:<10} default {default:<10} {bounds:<18} {source}"
                        ),
                    );
                }
            }
            other if other.starts_with("applejack_feature_set ") => {
                let mut words = other.split_whitespace().skip(1);
                let id = words.next().unwrap_or_default().to_owned();
                let state = words.next().unwrap_or_default().to_owned();

                match FEATURES.iter().find(|(known, _, _)| *known == id) {
                    // A core feature refuses by name and writes nothing.
                    Some((_, "core", _)) => emit(
                        &mut log,
                        "Features",
                        &format!("[Features] refused: '{id}' is core and cannot be toggled"),
                    ),
                    Some(_) => {
                        features.retain(|held| *held != id);
                        if state == "on" {
                            features.push(id.clone());
                        }
                        emit(
                            &mut log,
                            "Features",
                            &format!("[Features] {id} is now {state}"),
                        );
                    }
                    None => emit(
                        &mut log,
                        "Features",
                        &format!("[Features] refused: '{id}' is not a feature this build declares"),
                    ),
                }
            }
            other if other.starts_with("applejack_setting_set ") => {
                let mut words = other.split_whitespace().skip(1);
                let id = words.next().unwrap_or_default().to_owned();
                let value = words.next().unwrap_or_default().to_owned();

                if SETTINGS.iter().any(|(known, _, _, _)| *known == id) {
                    settings.retain(|(key, _)| *key != id);
                    settings.push((id.clone(), value.clone()));
                    emit(
                        &mut log,
                        "Settings",
                        &format!("[Settings] {id} is now {value}"),
                    );
                } else {
                    emit(
                        &mut log,
                        "Settings",
                        &format!("[Settings] refused: '{id}' is not a catalogued setting"),
                    );
                }
            }
            "applejack_storage" => {
                emit(
                    &mut log,
                    "Storage",
                    "[Storage] provider hosted, journalled - 0 owed write(s)",
                );
            }
            other if other.starts_with("kick ") => {
                let who = other.trim_start_matches("kick ").trim();
                let steam_id = 76561198000000000u64;
                emit(
                    &mut log,
                    "Network",
                    &format!("Kicking {who} [{steam_id}] - kicked by the host"),
                );
                connected = connected.saturating_sub(1);
            }
            "join" => {
                let steam_id = 76561198000000000u64 + connected as u64;
                emit(
                    &mut log,
                    "Network",
                    &format!("Player{connected} [{steam_id}] is connected"),
                );
                connected += 1;
            }
            "leave" => {
                if connected > 0 {
                    connected -= 1;
                    let steam_id = 76561198000000000u64 + connected as u64;
                    emit(
                        &mut log,
                        "Network",
                        &format!("Player{connected} [{steam_id}] disconnected"),
                    );
                }
            }
            other => {
                // The engine's own reply to an unknown command.
                emit(&mut log, "ConVar", &format!("Unknown command '{other}'"));
            }
        }

        status_bar(&options, connected as u32, started.elapsed().as_secs());
    }

    std::process::ExitCode::SUCCESS
}

/// Write a line in both the console and the file formats.
fn emit(log: &mut Option<std::fs::File>, logger: &str, message: &str) {
    let now = chrono::Local::now();

    // Console: 12-hour clock, logger padded or truncated to exactly 8.
    let mut short = logger.to_owned();
    short.truncate(8);
    println!("{} {:<8} {}", now.format("%I:%M:%S"), short, message);
    let _ = std::io::stdout().flush();

    if let Some(file) = log {
        // NLog writes `ffff`, four fractional digits. chrono has `%.3f`, `%.6f`
        // and `%.9f` and no four-digit form, and asking it for `%.4f` panics
        // inside `write_fmt` rather than failing to parse, so the fraction is
        // built by hand.
        let tenths_of_a_milli = now.timestamp_subsec_nanos() / 100_000;
        let _ = writeln!(
            file,
            "{}.{:04}\t[{}] {}\t",
            now.format("%Y/%m/%d %H:%M:%S"),
            tenths_of_a_milli,
            logger,
            message
        );
        let _ = file.flush();
    }
}

/// The in-place status bar, carriage-returned the way the console redraws it.
fn status_bar(options: &Options, players: u32, uptime: u64) {
    let (h, m, s) = (uptime / 3600, (uptime % 3600) / 60, uptime % 60);
    print!(
        "\r{} ({}/{}) [{h:02}:{m:02}:{s:02}]  Network: 0.42ms  Physics: 1.10ms  Update: 3.75ms\r\n",
        options.hostname, players, options.max_players
    );
    let _ = std::io::stdout().flush();
}

fn parse_args() -> Options {
    let mut options = Options {
        log_file: None,
        crash_after: None,
        hang: false,
        ignore_quit: false,
        players: 0,
        flood: false,
        hostname: "AppleJackRP Dev".to_owned(),
        max_players: 64,
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--log-file" => options.log_file = args.next().map(PathBuf::from),
            "--crash-after" => options.crash_after = args.next().and_then(|v| v.parse().ok()),
            "--hang" => options.hang = true,
            "--ignore-quit" => options.ignore_quit = true,
            "--flood" => options.flood = true,
            "--players" => options.players = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--hostname" => {
                if let Some(value) = args.next() {
                    options.hostname = value;
                }
            }
            // Accept and ignore the real server's flags, so the same config can
            // point at either binary.
            "+game"
            | "+hostname"
            | "+net_game_server_token"
            | "+port"
            | "+net_query_port"
            | "+net_hide_address" => {
                args.next();
            }
            _ => {}
        }
    }

    options
}

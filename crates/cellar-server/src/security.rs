use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AntiCheatType {
    pub name: String,
    pub state: String,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AntiCheatStatus {
    pub state: String,
    pub summary: String,
    pub types: Vec<AntiCheatType>,
}

pub async fn inspect(log_file: Option<&Path>) -> AntiCheatStatus {
    let Some(log_file) = log_file else {
        return unknown("no engine log is configured");
    };

    let mut text = String::new();
    let mut candidates = vec![log_file.to_owned()];
    if let Some(parent) = log_file.parent() {
        let sibling = parent.join("sbox-server.log");
        if sibling != *log_file {
            candidates.push(sibling);
        }
    }

    for path in candidates {
        if let Ok(bytes) = tokio::fs::read(path).await {
            let tail = bytes.len().saturating_sub(128 * 1024);
            text.push_str(&String::from_utf8_lossy(&bytes[tail..]));
        }
    }

    detect(&text)
}

pub fn detect(log: &str) -> AntiCheatStatus {
    let mut types = Vec::new();
    let lower = log.to_ascii_lowercase();

    if lower.contains("vac") || lower.contains("steamgameserver_init") {
        let (state, evidence) = if lower.contains("not be listed or vac protected")
            || lower.contains("unauthenticated mode")
            || lower.contains("vac disabled")
        {
            (
                "disabled",
                matching_line(log, |line| {
                    let line = line.to_ascii_lowercase();
                    line.contains("vac") || line.contains("unauthenticated mode")
                }),
            )
        } else if lower.contains("vac secure")
            || lower.contains("vac protected")
            || lower.contains("authenticated mode")
        {
            (
                "enabled",
                matching_line(log, |line| {
                    let line = line.to_ascii_lowercase();
                    line.contains("vac") || line.contains("authenticated mode")
                }),
            )
        } else {
            ("unknown", None)
        };
        types.push(AntiCheatType {
            name: "VAC".to_owned(),
            state: state.to_owned(),
            evidence,
        });
    }

    for (name, needles) in [
        (
            "Easy Anti-Cheat",
            &["easy anti-cheat", "easyanticheat", "eac"][..],
        ),
        ("BattlEye", &["battleye", "battle eye"][..]),
    ] {
        if needles.iter().any(|needle| contains_signal(&lower, needle)) {
            let disabled = lower.contains("disabled") || lower.contains("not running");
            types.push(AntiCheatType {
                name: name.to_owned(),
                state: if disabled { "disabled" } else { "enabled" }.to_owned(),
                evidence: matching_line(log, |line| {
                    let line = line.to_ascii_lowercase();
                    needles.iter().any(|needle| contains_signal(&line, needle))
                }),
            });
        }
    }

    let state = if types.iter().any(|kind| kind.state == "enabled") {
        "enabled"
    } else if !types.is_empty() && types.iter().all(|kind| kind.state == "disabled") {
        "disabled"
    } else {
        "unknown"
    };
    let summary = match state {
        "enabled" => "anti-cheat protection detected",
        "disabled" => "anti-cheat protection is disabled",
        _ if types.is_empty() => "no anti-cheat signal found in engine logs",
        _ => "anti-cheat state needs verification",
    };

    AntiCheatStatus {
        state: state.to_owned(),
        summary: summary.to_owned(),
        types,
    }
}

fn unknown(summary: &str) -> AntiCheatStatus {
    AntiCheatStatus {
        state: "unknown".to_owned(),
        summary: summary.to_owned(),
        types: Vec::new(),
    }
}

fn matching_line<F>(text: &str, predicate: F) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    text.lines().rev().find(|line| predicate(line)).map(|line| {
        let line = line.trim();
        if line.chars().count() > 240 {
            format!("{}…", line.chars().take(240).collect::<String>())
        } else {
            line.to_owned()
        }
    })
}

fn contains_signal(text: &str, signal: &str) -> bool {
    if signal.len() > 3 {
        return text.contains(signal);
    }
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .any(|word| word == signal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthenticated_steam_mode_reports_vac_disabled() {
        let status = detect(
            "SteamGameServer_Init: failed, falling back to unauthenticated mode - server will not be listed or VAC protected",
        );
        assert_eq!(status.state, "disabled");
        assert_eq!(status.types[0].name, "VAC");
        assert_eq!(status.types[0].state, "disabled");
    }

    #[test]
    fn secure_vac_and_battleye_are_reported() {
        let status = detect("VAC secure\nBattlEye service started");
        assert_eq!(status.state, "enabled");
        assert_eq!(status.types.len(), 2);
        assert!(status.types.iter().all(|kind| kind.state == "enabled"));
    }

    #[test]
    fn no_signal_is_honestly_unknown() {
        let status = detect("Lobby created - session is joinable");
        assert_eq!(status.state, "unknown");
        assert!(status.types.is_empty());
    }
}

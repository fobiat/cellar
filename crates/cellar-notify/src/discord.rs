//! Turning a batch of events into one Discord message.
//!
//! One embed per batch, not per event. A restart that disconnects everybody
//! produces one exit and a dozen leaves, and a dozen separate messages is how a
//! channel becomes unreadable at exactly the moment somebody needs to read it.

use cellar_core::event::{Event, LeaveReason};
use cellar_core::theme;

/// Embed colour as Discord wants it: one integer, `0xRRGGBB`.
fn colour(token: theme::Token) -> u32 {
    let (r, g, b) = theme::rgb(token.dark);
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// The worst thing in the batch decides the colour.
fn colour_for(batch: &[Event]) -> u32 {
    let has_fault = batch.iter().any(|e| {
        matches!(
            e,
            Event::ProcessExited {
                graceful: false,
                ..
            }
        ) || matches!(e, Event::BridgeHealth { healthy: false, .. })
    });

    if has_fault {
        return colour(theme::RUSSET);
    }

    let has_good_news = batch
        .iter()
        .any(|e| matches!(e, Event::ServerReady { .. } | Event::ProcessStarted { .. }));

    if has_good_news {
        colour(theme::ORCHARD)
    } else {
        colour(theme::AZURE)
    }
}

/// Compose the webhook body.
pub fn payload(batch: &[Event], hostname: &str) -> serde_json::Value {
    let lines: Vec<String> = batch.iter().filter_map(describe).collect();

    // Discord refuses an embed description over 4096 characters, and a refused
    // message is worse than a truncated one.
    let mut description = lines.join("\n");
    if description.chars().count() > 3900 {
        description = description.chars().take(3900).collect::<String>();
        description.push_str("\n… and more");
    }

    serde_json::json!({
        "username": format!("{} {}", theme::STAR, hostname),
        "embeds": [{
            "title": title_for(batch),
            "description": description,
            "color": colour_for(batch),
            "footer": { "text": theme::TAGLINE },
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }]
    })
}

fn title_for(batch: &[Event]) -> String {
    for event in batch {
        match event {
            Event::ProcessExited {
                graceful: false, ..
            } => return "Server stopped unexpectedly".into(),
            Event::ProcessExited { .. } => return "Server stopped".into(),
            Event::ServerReady { .. } => return "Server is up".into(),
            Event::BridgeHealth { healthy: false, .. } => {
                return "Database bridge is unhealthy".into();
            }
            _ => {}
        }
    }

    let joins = batch
        .iter()
        .filter(|e| matches!(e, Event::PlayerJoined { .. }))
        .count();
    let leaves = batch
        .iter()
        .filter(|e| matches!(e, Event::PlayerLeft { .. }))
        .count();

    match (joins, leaves) {
        (0, 0) => "Server activity".into(),
        (j, 0) => format!("{j} player(s) joined"),
        (0, l) => format!("{l} player(s) left"),
        (j, l) => format!("{j} joined, {l} left"),
    }
}

/// One line per event, or `None` for an event with nothing to say.
fn describe(event: &Event) -> Option<String> {
    Some(match event {
        Event::ProcessStarted { pid, .. } => format!("`starting` process {pid}"),
        Event::ServerReady { map, .. } => match map {
            Some(map) => format!("`ready` on {map}"),
            None => "`ready` accepting players".to_owned(),
        },
        Event::ProcessExited { code, graceful } => {
            let how = if *graceful {
                "stopped cleanly"
            } else {
                "exited unexpectedly"
            };
            match code {
                Some(code) => format!("`exit` {how}, code {code}"),
                None => format!("`exit` {how}, killed by a signal"),
            }
        }
        // Names are escaped: a player called `**everyone**` should not be able to
        // format the channel, and one containing a backtick should not break the
        // code span it sits in.
        Event::PlayerJoined { name, steam_id } => {
            format!("`join` {} ({steam_id})", escape(name))
        }
        Event::PlayerLeft {
            name,
            steam_id,
            reason,
        } => match reason {
            LeaveReason::Kicked { reason } if !reason.is_empty() => {
                format!("`kick` {} ({steam_id}): {}", escape(name), escape(reason))
            }
            LeaveReason::Kicked { .. } => format!("`kick` {} ({steam_id})", escape(name)),
            LeaveReason::Disconnected => format!("`left` {} ({steam_id})", escape(name)),
        },
        Event::BridgeHealth { healthy, detail } => {
            if *healthy {
                "`bridge` recovered".to_owned()
            } else {
                format!("`bridge` {}", escape(detail))
            }
        }
        Event::CommandDispatched { command, actor } => {
            format!("`console` {} ran `{}`", escape(actor), escape(command))
        }
        Event::CommandReplied { .. }
        | Event::Log(_)
        | Event::Status(_)
        | Event::Resources(_)
        | Event::Unparsed { .. } => return None,
    })
}

/// Neutralise Discord markdown in text somebody else chose.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '*' | '_' | '`' | '~' | '|' | '\\' | '>' | '@' | '#' | ':'
        ) {
            out.push('\\');
        }
        // A newline in a display name would break the line-per-event layout.
        out.push(if c == '\n' || c == '\r' { ' ' } else { c });
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_join_batch_is_one_message_with_one_line_each() {
        let batch = vec![
            Event::PlayerJoined {
                steam_id: 1,
                name: "Kyle".into(),
            },
            Event::PlayerJoined {
                steam_id: 2,
                name: "Applejack".into(),
            },
        ];

        let payload = payload(&batch, "AppleJackRP Dev");
        let embed = &payload["embeds"][0];

        assert_eq!(embed["title"], "2 player(s) joined");
        let description = embed["description"].as_str().unwrap();
        assert_eq!(description.lines().count(), 2);
        assert!(description.contains("Kyle"));
    }

    #[test]
    fn a_crash_colours_the_embed_russet_and_a_start_colours_it_orchard() {
        let crash = vec![Event::ProcessExited {
            code: Some(1),
            graceful: false,
        }];
        assert_eq!(colour_for(&crash), colour(theme::RUSSET));

        let up = vec![Event::ServerReady {
            hostname: None,
            map: None,
        }];
        assert_eq!(colour_for(&up), colour(theme::ORCHARD));

        let ordinary = vec![Event::PlayerJoined {
            steam_id: 1,
            name: "Kyle".into(),
        }];
        assert_eq!(colour_for(&ordinary), colour(theme::AZURE));
    }

    #[test]
    fn the_azure_the_brand_states_is_the_azure_that_is_sent() {
        assert_eq!(colour(theme::AZURE), 0x2F8FE0);
    }

    /// A display name is chosen by the account holder, so it is text, not markup.
    #[test]
    fn a_player_cannot_format_the_channel_with_their_name() {
        let batch = vec![Event::PlayerJoined {
            steam_id: 1,
            name: "**@everyone** `hi`".into(),
        }];

        let payload = payload(&batch, "test");
        let description = payload["embeds"][0]["description"].as_str().unwrap();

        assert!(!description.contains("**@everyone**"));
        assert!(description.contains("\\*\\*\\@everyone\\*\\*"));
    }

    #[test]
    fn a_newline_in_a_name_cannot_forge_extra_lines() {
        let batch = vec![Event::PlayerJoined {
            steam_id: 1,
            name: "Kyle\n`exit` server destroyed".into(),
        }];

        let payload = payload(&batch, "test");
        let description = payload["embeds"][0]["description"].as_str().unwrap();
        assert_eq!(description.lines().count(), 1);
    }

    #[test]
    fn a_huge_batch_is_truncated_rather_than_refused() {
        let batch: Vec<Event> = (0..2000)
            .map(|i| Event::PlayerJoined {
                steam_id: i,
                name: format!("player-with-a-long-name-{i}"),
            })
            .collect();

        let payload = payload(&batch, "test");
        let description = payload["embeds"][0]["description"].as_str().unwrap();

        assert!(description.chars().count() <= 3920);
        assert!(description.ends_with("and more"));
    }

    #[test]
    fn events_with_nothing_to_say_produce_no_line() {
        assert!(
            describe(&Event::Status(cellar_core::event::StatusBar {
                hostname: "x".into(),
                players: 0,
                max_players: 64,
                uptime_seconds: 0,
                network_ms: None,
                physics_ms: None,
                navmesh_ms: None,
                animation_ms: None,
                update_ms: None,
            }))
            .is_none()
        );
    }

    #[test]
    fn a_graceful_stop_reads_differently_from_a_crash() {
        let clean = describe(&Event::ProcessExited {
            code: Some(0),
            graceful: true,
        })
        .unwrap();
        assert!(clean.contains("cleanly"));

        let crash = describe(&Event::ProcessExited {
            code: None,
            graceful: false,
        })
        .unwrap();
        assert!(crash.contains("signal"));
    }
}

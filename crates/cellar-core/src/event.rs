//! The one event vocabulary. Everything downstream (webhooks, the TUI, the web
//! UI, the ops tables) consumes this rather than raw log lines, so a change to
//! an engine log string reaches exactly one module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A Steam account id as the engine prints it.
pub type SteamId = u64;

/// A SteamID on the wire is a string, because JSON numbers are doubles.
///
/// Every real SteamID64 is above 2^53, so `JSON.parse` rounds one to the
/// nearest multiple of 16 and hands the browser a different account. It looked
/// right for years: the id is 17 digits either way, and only the last one or
/// two move. Measured against three connected players whose ids differ by one,
/// the dashboard showed all three as the same person and the kick button sent
/// that id back.
///
/// Serialising as a string is what Steam's own Web API does, and for this
/// reason. Deserialising accepts either, so an older payload still reads.
pub mod steam_id_wire {
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        match Wire::deserialize(deserializer)? {
            Wire::Text(text) => text.parse().map_err(de::Error::custom),
            Wire::Number(number) => Ok(number),
        }
    }

    /// The optional half, for a record that may not name an account.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer, de};

        pub fn serialize<S: Serializer>(
            value: &Option<u64>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                Some(id) => serializer.serialize_str(&id.to_string()),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<u64>, D::Error> {
            match Option::<super::Wire>::deserialize(deserializer)? {
                Some(super::Wire::Text(text)) => text.parse().map(Some).map_err(de::Error::custom),
                Some(super::Wire::Number(number)) => Ok(Some(number)),
                None => Ok(None),
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Text(String),
        Number(u64),
    }
}

/// Severity as the engine's own logger classifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Where a line came from. The two channels carry different fidelity, and a
/// consumer sometimes needs to know which one it is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The pseudo-terminal the server was spawned on. Truncates logger names to
    /// eight characters and carries no date.
    Console,
    /// `logs/sbox-server.log`. Tab separated, dated, complete logger names.
    LogFile,
    /// Cellar itself, not the child.
    Cellar,
}

/// One parsed line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub at: DateTime<Utc>,
    pub level: Level,
    /// Engine logger category, for example `Identity` or `Storage`. Truncated to
    /// eight characters when `origin` is `Console`.
    pub logger: String,
    pub message: String,
    pub origin: Origin,

    /// Which bucket of the console's category filter this falls into.
    ///
    /// Decided once, here, by the gamemode's profile. The browser used to
    /// reimplement the rule in JavaScript and the two copies had already
    /// diverged on the gamemode arm. `default` so a line deserialised from an
    /// older recording still parses.
    #[serde(default = "other_category")]
    pub category: crate::profile::Category,
}

fn other_category() -> crate::profile::Category {
    crate::profile::Category::Other
}

/// Why a player left, as far as the log can tell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaveReason {
    Disconnected,
    Kicked { reason: String },
}

/// What the supervisor observed.
///
/// `Unparsed` is deliberate: a line the grammar does not recognise is counted
/// and surfaced, never dropped. A parser that quietly stops matching after an
/// engine update is the failure mode worth engineering against.
// Not `Eq`: the status bar and the resource sample both carry frame timings as
// floats, and a float has no total equality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// The child process was spawned. Not yet serving.
    ProcessStarted { pid: u32, command: String },
    /// The engine reached the point where it accepts connections.
    ServerReady {
        hostname: Option<String>,
        map: Option<String>,
    },
    /// The child exited. `code` is `None` when a signal killed it.
    ProcessExited { code: Option<i32>, graceful: bool },

    PlayerJoined {
        #[serde(with = "steam_id_wire")]
        steam_id: SteamId,
        name: String,
    },
    PlayerLeft {
        #[serde(with = "steam_id_wire")]
        steam_id: SteamId,
        name: String,
        reason: LeaveReason,
    },

    /// A line the grammar recognised as engine or gamemode output.
    Log(LogLine),
    /// A line nothing matched. Counted, surfaced, never discarded.
    Unparsed { raw: String, origin: Origin },

    /// The status bar the dedicated console draws, sampled.
    Status(StatusBar),
    /// Process resource sample, the htop input.
    Resources(ResourceSample),

    /// A console command Cellar dispatched, and what came back.
    CommandDispatched { command: String, actor: String },
    CommandReplied {
        command: String,
        reply: Vec<String>,
        ok: bool,
    },

    /// The bridge's own health, so the UI can show it beside the server's.
    BridgeHealth { healthy: bool, detail: String },
}

/// An [`Event`] with the instance it came from.
///
/// A wrapper rather than a field on `Event`, because `Event` is internally
/// tagged and consumed by four independent things: the tracker, the TUI, the
/// notifier, and the browser, which switches on `kind`. `flatten` over an
/// internally-tagged enum produces exactly the JSON the dashboard already
/// parses plus one `instance` key, so nothing downstream has to change to keep
/// working and everything downstream gains the ability to filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceEvent {
    pub instance: crate::config::InstanceId,
    #[serde(flatten)]
    pub event: Event,
}

/// The dedicated server console's status line, which is the only place the
/// engine reports its own frame timings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusBar {
    pub hostname: String,
    pub players: u32,
    pub max_players: u32,
    /// True engine uptime, with the bar's rounded hour corrected. See
    /// [`crate::statusbar`].
    pub uptime_seconds: u64,
    pub network_ms: Option<f32>,
    pub physics_ms: Option<f32>,
    pub navmesh_ms: Option<f32>,
    pub animation_ms: Option<f32>,
    pub update_ms: Option<f32>,
}

/// A resource sample over the whole process tree.
///
/// The tree, not the direct child: under Wine the process Cellar spawns is
/// `wine`, and the memory and cpu that matter belong to `sbox-server.exe`
/// beneath it. Sampling only the child reports near zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ResourceSample {
    pub at: DateTime<Utc>,
    /// Percent of one core, so a four-thread server reads above 100.
    pub cpu_percent: f32,
    /// Process-tree CPU normalized across all logical host cores.
    #[serde(default)]
    pub cpu_percent_all_cores: f32,
    /// Logical host cores used to normalize process CPU.
    #[serde(default)]
    pub cpu_core_count: usize,
    pub memory_bytes: u64,
    pub process_count: usize,
    /// Host-wide CPU percentage, normalized to the whole machine.
    pub host_cpu_percent: f32,
    /// Host-wide memory percentage.
    pub host_memory_percent: f32,
    /// Network traffic observed since the previous sample.
    pub network_rx_bytes_per_sec: u64,
    pub network_tx_bytes_per_sec: u64,
}

impl Event {
    /// A short, stable label for counters and the ops tables.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ProcessStarted { .. } => "process_started",
            Self::ServerReady { .. } => "server_ready",
            Self::ProcessExited { .. } => "process_exited",
            Self::PlayerJoined { .. } => "player_joined",
            Self::PlayerLeft { .. } => "player_left",
            Self::Log(_) => "log",
            Self::Unparsed { .. } => "unparsed",
            Self::Status(_) => "status",
            Self::Resources(_) => "resources",
            Self::CommandDispatched { .. } => "command_dispatched",
            Self::CommandReplied { .. } => "command_replied",
            Self::BridgeHealth { .. } => "bridge_health",
        }
    }

    /// Whether this is worth a webhook. High-frequency samples are not.
    pub fn is_notable(&self) -> bool {
        !matches!(
            self,
            Self::Log(_) | Self::Status(_) | Self::Resources(_) | Self::Unparsed { .. }
        )
    }

    /// What this event is worth writing into the operations record, beyond its
    /// kind: a logger, an account, and a sentence a person can read.
    ///
    /// Every notable event used to be stored as a bare kind and a timestamp,
    /// because the recorder passed `None` for all three. Nothing read the table
    /// back, so nothing noticed that "a player joined" recorded neither who nor
    /// when they left. The activity screen is what made it visible.
    ///
    /// The payload is a sentence rather than the serialised event. An audit row
    /// is read by a person asking what happened at 21:04, and a JSON blob of the
    /// variant's fields answers that worse than one line does. The full event
    /// is still on the websocket for anything that wants structure.
    pub fn record(&self) -> EventRecord<'_> {
        match self {
            Self::ProcessStarted { pid, command } => EventRecord {
                logger: Some("supervisor"),
                steam_id: None,
                detail: Some(format!("pid {pid}: {command}")),
            },
            Self::ServerReady { hostname, map } => EventRecord {
                logger: Some("supervisor"),
                steam_id: None,
                detail: Some(match (hostname.as_deref(), map.as_deref()) {
                    (Some(host), Some(map)) => format!("{host} on {map}"),
                    (Some(host), None) => host.to_owned(),
                    (None, Some(map)) => format!("on {map}"),
                    (None, None) => "serving".to_owned(),
                }),
            },
            Self::ProcessExited { code, graceful } => EventRecord {
                logger: Some("supervisor"),
                steam_id: None,
                detail: Some(match (code, graceful) {
                    (Some(code), true) => format!("stopped as asked, exit {code}"),
                    (Some(code), false) => {
                        format!("exited with code {code} without being asked to")
                    }
                    (None, true) => "stopped as asked, killed after the grace period".to_owned(),
                    (None, false) => "killed by a signal".to_owned(),
                }),
            },
            Self::PlayerJoined { steam_id, name } => EventRecord {
                logger: Some("players"),
                steam_id: Some(*steam_id),
                detail: Some(format!("{name} joined")),
            },
            Self::PlayerLeft {
                steam_id,
                name,
                reason,
            } => EventRecord {
                logger: Some("players"),
                steam_id: Some(*steam_id),
                // Not `leave_reason_label`, which is deliberately short for its
                // `VARCHAR(32)` column and drops the kick reason. Why somebody
                // was kicked is most of what an audit row about a kick is for.
                detail: Some(match reason {
                    LeaveReason::Disconnected => format!("{name} disconnected"),
                    LeaveReason::Kicked { reason } => format!("{name} was kicked: {reason}"),
                }),
            },
            Self::CommandDispatched { command, actor } => EventRecord {
                logger: Some("console"),
                steam_id: None,
                detail: Some(format!("{actor} ran {command}")),
            },
            Self::CommandReplied { command, ok, .. } => EventRecord {
                logger: Some("console"),
                steam_id: None,
                detail: Some(format!(
                    "{command} {}",
                    if *ok { "replied" } else { "was refused" }
                )),
            },
            Self::BridgeHealth { healthy, detail } => EventRecord {
                logger: Some("bridge"),
                steam_id: None,
                detail: Some(format!(
                    "{}: {detail}",
                    if *healthy { "healthy" } else { "unhealthy" }
                )),
            },
            // The three that are never notable, plus `Log`. Reached only if
            // somebody records one deliberately.
            Self::Log(_) | Self::Status(_) | Self::Resources(_) | Self::Unparsed { .. } => {
                EventRecord {
                    logger: None,
                    steam_id: None,
                    detail: None,
                }
            }
        }
    }
}

/// The parts of an [`Event`] the operations record keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord<'a> {
    pub logger: Option<&'a str>,
    pub steam_id: Option<u64>,
    /// One readable sentence, not the serialised variant.
    pub detail: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod instance_event_tests {
    use super::*;
    use crate::config::InstanceId;

    fn wrap(event: Event) -> serde_json::Value {
        let wrapped = InstanceEvent {
            instance: InstanceId::new("published").unwrap(),
            event,
        };
        serde_json::to_value(wrapped).unwrap()
    }

    /// A wire-format assumption rather than an obvious truth. `flatten` over an
    /// internally-tagged enum could have nested the variant instead, and the
    /// browser switches on a top-level `kind`.
    #[test]
    fn kind_stays_top_level_for_every_variant_shape() {
        let cases = [
            Event::ProcessStarted {
                pid: 1,
                command: "a".into(),
            },
            Event::ServerReady {
                hostname: None,
                map: None,
            },
            Event::Unparsed {
                raw: "a".into(),
                origin: Origin::Cellar,
            },
            Event::Status(StatusBar::default()),
        ];

        for event in cases {
            let json = wrap(event);
            assert!(json.get("kind").is_some(), "{json}");
            assert_eq!(json["instance"], "published");
        }
    }

    /// A SteamID is a string on the wire, and the reason is the reader.
    ///
    /// This test used to assert the opposite, that the id stayed a JSON
    /// number through `flatten`. It did, and that was the defect: serde keeps
    /// a u64 exact, and then `JSON.parse` in the browser turns it into a
    /// double and rounds it to the nearest multiple of 16. The test proved the
    /// hop it checked and stopped one short of the consumer that breaks.
    #[test]
    fn a_seventeen_digit_steam_id_reaches_a_double_precision_reader_intact() {
        let json = wrap(Event::PlayerJoined {
            steam_id: 76561198000000001,
            name: "Kyle".into(),
        });

        assert_eq!(json["steam_id"], serde_json::json!("76561198000000001"));

        // What the browser does with it, done here: every real SteamID64 is
        // above 2^53, so a number would come back as ...000 rather than ...001.
        let text = serde_json::to_string(&json).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(reparsed["steam_id"].as_str(), Some("76561198000000001"));
        assert!(76561198000000001f64 as u64 != 76561198000000001);
    }

    /// The wire form reads back, including the number an older payload holds.
    #[test]
    fn a_steam_id_deserialises_from_a_string_or_a_number() {
        for wire in [r#""76561198000000001""#, "76561198000000001"] {
            let json = format!(r#"{{"kind":"player_joined","steam_id":{wire},"name":"Kyle"}}"#);
            let event: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(
                event,
                Event::PlayerJoined {
                    steam_id: 76561198000000001,
                    name: "Kyle".into(),
                }
            );
        }
    }

    #[test]
    fn it_round_trips_back_into_the_same_event() {
        let event = Event::ProcessExited {
            code: Some(137),
            graceful: false,
        };
        let json = serde_json::to_string(&InstanceEvent {
            instance: InstanceId::new("dev").unwrap(),
            event: event.clone(),
        })
        .unwrap();

        let back: InstanceEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event, event);
        assert_eq!(back.instance.as_str(), "dev");
    }
}

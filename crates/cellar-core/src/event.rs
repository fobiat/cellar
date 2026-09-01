//! The one event vocabulary. Everything downstream (webhooks, the TUI, the web
//! UI, the ops tables) consumes this rather than raw log lines, so a change to
//! an engine log string reaches exactly one module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A Steam account id as the engine prints it.
pub type SteamId = u64;

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
    ProcessStarted {
        pid: u32,
        command: String,
    },
    /// The engine reached the point where it accepts connections.
    ServerReady {
        hostname: Option<String>,
        map: Option<String>,
    },
    /// The child exited. `code` is `None` when a signal killed it.
    ProcessExited {
        code: Option<i32>,
        graceful: bool,
    },

    PlayerJoined {
        steam_id: SteamId,
        name: String,
    },
    PlayerLeft {
        steam_id: SteamId,
        name: String,
        reason: LeaveReason,
    },

    /// A line the grammar recognised as engine or gamemode output.
    Log(LogLine),
    /// A line nothing matched. Counted, surfaced, never discarded.
    Unparsed {
        raw: String,
        origin: Origin,
    },

    /// The status bar the dedicated console draws, sampled.
    Status(StatusBar),
    /// Process resource sample, the htop input.
    Resources(ResourceSample),

    /// A console command Cellar dispatched, and what came back.
    CommandDispatched {
        command: String,
        actor: String,
    },
    CommandReplied {
        command: String,
        reply: Vec<String>,
        ok: bool,
    },

    /// The bridge's own health, so the UI can show it beside the server's.
    BridgeHealth {
        healthy: bool,
        detail: String,
    },
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

    /// `flatten` forces serde's buffering path, and a SteamID is a u64 above
    /// 2^53. A 17-digit id arriving as a float would be a different player.
    #[test]
    fn a_seventeen_digit_steam_id_survives_the_flatten() {
        let json = wrap(Event::PlayerJoined {
            steam_id: 76561198000000001,
            name: "Kyle".into(),
        });

        assert_eq!(json["steam_id"], serde_json::json!(76561198000000001u64));
        assert_eq!(json["steam_id"].to_string(), "76561198000000001");
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

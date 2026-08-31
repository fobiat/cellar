//! The one state snapshot the CLI, the TUI and the web UI all render.
//!
//! Three interfaces that each computed their own view of "is it up, who is on
//! it" would drift, and the drift would show as two screens disagreeing in
//! front of an operator trying to decide something. So the supervisor owns one
//! of these and everything else formats it.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::event::{Event, LeaveReason, LogLine, ResourceSample, StatusBar, SteamId};
use crate::lifecycle::State;

/// How many resource samples to keep for the sparklines.
pub const SAMPLE_HISTORY: usize = 240;

/// How many log lines to keep in memory for the live panes.
pub const LOG_HISTORY: usize = 2000;

/// A player currently on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub steam_id: SteamId,
    pub name: String,
    pub joined_at: DateTime<Utc>,
}

impl Player {
    pub fn connected_seconds(&self, now: DateTime<Utc>) -> i64 {
        (now - self.joined_at).num_seconds().max(0)
    }
}

/// Health of the bridge, as its own routes have observed it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeStats {
    pub enabled: bool,
    pub healthy: bool,
    pub reads: u64,
    pub writes: u64,
    pub absent: u64,
    pub refused: u64,
    /// Writes that would have been a revision conflict, had the bridge answered
    /// 409. Counted rather than enforced: the shipped gamemode client cannot act
    /// on a conflict yet, so answering one would turn a recoverable write into a
    /// lost one.
    pub would_conflict: u64,
    pub last_error: Option<String>,
}

/// How the last run of the server ended.
///
/// Kept after the process is gone. "Stopped" on its own is an absence rather
/// than an answer, and the difference between exit 0, exit 137 and a signal
/// with no code at all is the difference between a clean stop, an out-of-memory
/// kill and something else killing the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    /// `None` when the process died on a signal and reported no code.
    pub code: Option<i32>,
    /// Whether Cellar asked for this exit.
    pub graceful: bool,
    pub at: DateTime<Utc>,
}

/// Everything an interface needs to draw a screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub state: State,
    /// How the last run ended, if one has. Survives the process.
    pub last_exit: Option<Exit>,
    pub hostname: String,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub players: Vec<Player>,
    pub max_players: u32,
    pub status_bar: Option<StatusBar>,
    pub resources: Option<ResourceSample>,
    pub resource_history: Vec<ResourceSample>,
    pub bridge: BridgeStats,
    pub consecutive_failures: u32,
    /// Lines the grammar did not recognise. A rising number means an engine
    /// update moved a log string and the parser needs revisiting.
    pub unparsed_lines: u64,
    pub restarts: u64,
}

impl Snapshot {
    pub fn uptime_seconds(&self, now: DateTime<Utc>) -> i64 {
        self.started_at
            .map(|at| (now - at).num_seconds().max(0))
            .unwrap_or(0)
    }
}

/// The supervisor's live state, folded from the event stream.
///
/// Kept pure so "a player joined twice", "a player left who was never here" and
/// "the server restarted with players still listed" are unit tests.
#[derive(Debug, Clone)]
pub struct Tracker {
    state: State,
    last_exit: Option<Exit>,
    hostname: String,
    pid: Option<u32>,
    started_at: Option<DateTime<Utc>>,
    players: Vec<Player>,
    max_players: u32,
    status_bar: Option<StatusBar>,
    samples: VecDeque<ResourceSample>,
    logs: VecDeque<LogLine>,
    bridge: BridgeStats,
    unparsed_lines: u64,
    restarts: u64,
    consecutive_failures: u32,
}

impl Tracker {
    pub fn new(hostname: impl Into<String>, max_players: u32) -> Self {
        Self {
            state: State::Stopped,
            last_exit: None,
            hostname: hostname.into(),
            pid: None,
            started_at: None,
            players: Vec::new(),
            max_players,
            status_bar: None,
            samples: VecDeque::with_capacity(SAMPLE_HISTORY),
            logs: VecDeque::with_capacity(LOG_HISTORY),
            bridge: BridgeStats::default(),
            unparsed_lines: 0,
            restarts: 0,
            consecutive_failures: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn set_state(&mut self, state: State) {
        self.state = state;
    }

    pub fn set_consecutive_failures(&mut self, failures: u32) {
        self.consecutive_failures = failures;
    }

    pub fn bridge_mut(&mut self) -> &mut BridgeStats {
        &mut self.bridge
    }

    pub fn logs(&self) -> impl Iterator<Item = &LogLine> {
        self.logs.iter()
    }

    pub fn samples(&self) -> impl Iterator<Item = &ResourceSample> {
        self.samples.iter()
    }

    pub fn players(&self) -> &[Player] {
        &self.players
    }

    /// Fold one event into the state.
    pub fn apply(&mut self, event: &Event, now: DateTime<Utc>) {
        match event {
            Event::ProcessStarted { pid, .. } => {
                self.pid = Some(*pid);
                self.started_at = Some(now);
                self.state = State::Starting;
                self.last_exit = None;
                // A restart cannot carry players over. Anything still listed is
                // a leak from the previous run, not somebody connected.
                self.players.clear();
                self.status_bar = None;
            }
            Event::ServerReady { hostname, .. } => {
                if let Some(name) = hostname {
                    self.hostname = name.clone();
                }
                self.state = State::Running;
            }
            Event::ProcessExited { code, graceful } => {
                self.last_exit = Some(Exit {
                    code: *code,
                    graceful: *graceful,
                    at: now,
                });
                self.pid = None;
                self.players.clear();
                self.status_bar = None;
                self.restarts += 1;
            }
            Event::PlayerJoined { steam_id, name } => {
                // The engine can repeat a connection line on a reconnect within
                // the same session; the roster is keyed by account, not by line.
                if let Some(existing) = self.players.iter_mut().find(|p| p.steam_id == *steam_id) {
                    existing.name = name.clone();
                    existing.joined_at = now;
                } else {
                    self.players.push(Player {
                        steam_id: *steam_id,
                        name: name.clone(),
                        joined_at: now,
                    });
                }
            }
            Event::PlayerLeft { steam_id, .. } => {
                self.players.retain(|p| p.steam_id != *steam_id);
            }
            Event::Status(bar) => {
                self.max_players = bar.max_players;
                self.hostname = bar.hostname.clone();
                self.status_bar = Some(bar.clone());
            }
            Event::Resources(sample) => {
                if self.samples.len() == SAMPLE_HISTORY {
                    self.samples.pop_front();
                }
                self.samples.push_back(*sample);
            }
            Event::Log(line) => {
                if self.logs.len() == LOG_HISTORY {
                    self.logs.pop_front();
                }
                self.logs.push_back(line.clone());
            }
            Event::Unparsed { .. } => self.unparsed_lines += 1,
            Event::BridgeHealth { healthy, detail } => {
                self.bridge.healthy = *healthy;
                self.bridge.last_error = (!healthy).then(|| detail.clone());
            }
            Event::CommandDispatched { .. } | Event::CommandReplied { .. } => {}
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            state: self.state,
            last_exit: self.last_exit,
            hostname: self.hostname.clone(),
            pid: self.pid,
            started_at: self.started_at,
            players: self.players.clone(),
            max_players: self.max_players,
            status_bar: self.status_bar.clone(),
            resources: self.samples.back().copied(),
            resource_history: self.samples.iter().copied().collect(),
            bridge: self.bridge.clone(),
            consecutive_failures: self.consecutive_failures,
            unparsed_lines: self.unparsed_lines,
            restarts: self.restarts.saturating_sub(0),
        }
    }
}

/// A leave for somebody who was never listed, for the tests to name.
pub fn is_unknown_leave(players: &[Player], steam_id: SteamId) -> bool {
    !players.iter().any(|p| p.steam_id == steam_id)
}

/// Convenience for the ops tables: describe a leave in one word.
pub fn leave_reason_label(reason: &LeaveReason) -> &'static str {
    match reason {
        LeaveReason::Disconnected => "disconnected",
        LeaveReason::Kicked { .. } => "kicked",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::event::Origin;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn tracker() -> Tracker {
        Tracker::new("AppleJackRP Dev", 64)
    }

    #[test]
    fn a_join_then_a_leave_empties_the_roster() {
        let mut t = tracker();
        t.apply(
            &Event::PlayerJoined {
                steam_id: 7,
                name: "Kyle".into(),
            },
            now(),
        );
        assert_eq!(t.players().len(), 1);

        t.apply(
            &Event::PlayerLeft {
                steam_id: 7,
                name: "Kyle".into(),
                reason: LeaveReason::Disconnected,
            },
            now(),
        );
        assert!(t.players().is_empty());
    }

    #[test]
    fn a_repeated_join_updates_rather_than_duplicates() {
        let mut t = tracker();
        t.apply(
            &Event::PlayerJoined {
                steam_id: 7,
                name: "Kyle".into(),
            },
            now(),
        );
        t.apply(
            &Event::PlayerJoined {
                steam_id: 7,
                name: "Kyle (renamed)".into(),
            },
            now(),
        );

        assert_eq!(t.players().len(), 1);
        assert_eq!(t.players()[0].name, "Kyle (renamed)");
    }

    #[test]
    fn a_leave_for_somebody_never_seen_is_harmless() {
        let mut t = tracker();
        t.apply(
            &Event::PlayerLeft {
                steam_id: 999,
                name: "Ghost".into(),
                reason: LeaveReason::Disconnected,
            },
            now(),
        );
        assert!(t.players().is_empty());
    }

    /// The bug this exists to prevent: a restart leaving a stale roster on
    /// screen, so an operator kicks somebody who is not connected.
    #[test]
    fn a_restart_clears_the_roster() {
        let mut t = tracker();
        t.apply(
            &Event::PlayerJoined {
                steam_id: 7,
                name: "Kyle".into(),
            },
            now(),
        );
        t.apply(
            &Event::ProcessExited {
                code: Some(1),
                graceful: false,
            },
            now(),
        );
        assert!(t.players().is_empty());

        t.apply(
            &Event::PlayerJoined {
                steam_id: 7,
                name: "Kyle".into(),
            },
            now(),
        );
        t.apply(
            &Event::ProcessStarted {
                pid: 42,
                command: "wine sbox-server.exe".into(),
            },
            now(),
        );
        assert!(
            t.players().is_empty(),
            "a fresh process starts with nobody on it"
        );
    }

    #[test]
    fn sample_and_log_history_are_bounded() {
        let mut t = tracker();

        for _ in 0..(SAMPLE_HISTORY + 50) {
            t.apply(
                &Event::Resources(ResourceSample {
                    at: now(),
                    cpu_percent: 1.0,
                    cpu_percent_all_cores: 0.5,
                    cpu_core_count: 2,
                    memory_bytes: 1,
                    process_count: 1,
                    host_cpu_percent: 1.0,
                    host_memory_percent: 1.0,
                    network_rx_bytes_per_sec: 0,
                    network_tx_bytes_per_sec: 0,
                }),
                now(),
            );
        }
        assert_eq!(t.samples().count(), SAMPLE_HISTORY);

        for _ in 0..(LOG_HISTORY + 50) {
            t.apply(
                &Event::Log(LogLine {
                    at: now(),
                    level: crate::event::Level::Info,
                    logger: "Identity".into(),
                    message: "x".into(),
                    origin: Origin::LogFile,
                }),
                now(),
            );
        }
        assert_eq!(t.logs().count(), LOG_HISTORY);
    }

    #[test]
    fn unparsed_lines_are_counted_so_a_broken_grammar_is_visible() {
        let mut t = tracker();
        for _ in 0..3 {
            t.apply(
                &Event::Unparsed {
                    raw: "???".into(),
                    origin: Origin::Console,
                },
                now(),
            );
        }
        assert_eq!(t.snapshot().unparsed_lines, 3);
    }

    #[test]
    fn readiness_moves_the_state_and_uptime_counts_from_the_start() {
        let mut t = tracker();
        t.apply(
            &Event::ProcessStarted {
                pid: 1,
                command: "x".into(),
            },
            now(),
        );
        assert_eq!(t.state(), State::Starting);

        t.apply(
            &Event::ServerReady {
                hostname: None,
                map: None,
            },
            now(),
        );
        assert_eq!(t.state(), State::Running);

        let later = now() + chrono::Duration::seconds(90);
        assert_eq!(t.snapshot().uptime_seconds(later), 90);
    }
}

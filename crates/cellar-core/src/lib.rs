//! Cellar's pure half.
//!
//! Nothing here does I/O, spawns a process, opens a socket or touches a
//! database. That is the point: the parts of a server manager that are easy to
//! get quietly wrong (what counts as a player joining, when to stop restarting,
//! which document keys are legal, what a secret must never do) are functions
//! over data, and their tests run in milliseconds with no game server, no Wine
//! and no MySQL anywhere near them.

pub mod ansi;
pub mod config;
pub mod doc_key;
pub mod event;
pub mod grammar;
pub mod lifecycle;
pub mod secret;
pub mod snapshot;
pub mod theme;

pub use config::{AuthMode, Config, ConfigError, Launcher};
pub use event::{Event, Level, LogLine, Origin, ResourceSample, StatusBar, SteamId};
pub use lifecycle::{BackoffPolicy, Decision, RestartPolicy, RestartTracker, State};
pub use secret::Secret;

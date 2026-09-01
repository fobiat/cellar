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
pub mod convar;
pub mod doc_key;
pub mod event;
pub mod grammar;
pub mod lifecycle;
pub mod profile;
pub mod secret;
pub mod snapshot;
pub mod statusbar;
pub mod theme;

/// The header `/healthz` answers with, so one Cellar can recognise another.
///
/// A header rather than the body, because `/healthz` bodies get matched by
/// probe configurations and the body has said `ok` since the route existed.
/// Nothing here is a secret: the endpoint is unauthenticated on purpose, and
/// the only claim made is "this is a Cellar", which any caller reaching it has
/// already worked out.
pub const HEALTH_HEADER: &str = "x-cellar";

pub use config::{AuthMode, Config, ConfigError, Launcher};
pub use event::{Event, Level, LogLine, Origin, ResourceSample, StatusBar, SteamId};
pub use lifecycle::{BackoffPolicy, Decision, RestartPolicy, RestartTracker, State};
pub use profile::{GamemodeProfile, ProfileCheck, ProfileCommand};
pub use secret::Secret;

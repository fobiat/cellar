//! Cellar's I/O half: the process, its terminal, its log file and its metrics.
//!
//! The one thing worth knowing before reading any of it: the server is spawned
//! on a pseudo-terminal, not on pipes, because the engine only builds its
//! interactive console when its output is not redirected. See [`process`].

pub mod hosting;
pub mod launch;
pub mod logfile;
pub mod metrics;
pub mod process;
pub mod supervisor;

pub use launch::{Command, command_for, log_file_for};
pub use supervisor::{Control, Handle, Supervisor};

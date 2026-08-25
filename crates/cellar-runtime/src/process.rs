//! Spawning the server on a pseudo-terminal.
//!
//! Not on pipes. `Launcher.cs` only constructs the dedicated console when
//! `Console.IsOutputRedirected` is false, and `ConsoleInput.cs` reads stdin with
//! `Console.ReadKey()` guarded by `Console.BufferWidth > 0`. Both of those fail
//! on a redirected stream, so a supervisor that owns the child's stdio through
//! pipes gets no console at all: nothing reads its stdin, and `quit`, `kick` and
//! `status` become unreachable.
//!
//! A pty satisfies both checks, because a pty *is* a terminal. It is also the
//! reason the Kubernetes deployment needs `stdin: true, tty: true`.
//!
//! The cost is that the stream carries the console's colouring and its in-place
//! status bar redraws. `cellar_core::ansi` handles that.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

use crate::launch::Command;

/// Terminal size given to the child.
///
/// Width matters: the engine asks for `Console.BufferWidth` before it will read
/// input at all, and it positions the status bar relative to the width. A
/// generous, fixed size keeps the layout stable regardless of whether Cellar
/// itself is attached to a terminal.
pub const PTY_COLS: u16 = 200;
pub const PTY_ROWS: u16 = 50;

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("could not open a pseudo-terminal: {0}")]
    OpenPty(String),

    #[error("could not spawn {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: anyhow_shim::Error,
    },

    #[error("could not attach to the pseudo-terminal: {0}")]
    Attach(String),
}

/// `portable-pty` returns `anyhow::Error`; this keeps that out of the public API
/// without taking a dependency on anyhow across the whole crate.
pub mod anyhow_shim {
    /// An error from the pty layer, flattened to its message.
    #[derive(Debug, thiserror::Error)]
    #[error("{0}")]
    pub struct Error(pub String);
}

/// What the reader thread sends back.
#[derive(Debug)]
pub enum Output {
    /// Raw bytes as they came off the terminal.
    Bytes(Vec<u8>),
    /// The child closed the terminal. No more output is coming.
    Eof,
}

/// A running server, its terminal, and the thread reading it.
pub struct Child {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Held so the master side stays open for the lifetime of the child.
    _master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pid: Option<u32>,
    command_line: String,
}

impl Child {
    /// The child's process id, when the platform reports one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The command as spawned, already redacted.
    pub fn command_line(&self) -> &str {
        &self.command_line
    }

    /// Type a line into the server's console.
    ///
    /// The engine reads character by character and dispatches on Enter, so the
    /// trailing newline is what makes this a command rather than a prefix.
    /// `ConVarSystem.Run` is called with `allowProtected: true` here, which is
    /// why this reaches native commands (`quit`, `kick`, `status`) that gamemode
    /// C# is refused.
    pub fn send_command(&self, command: &str) -> std::io::Result<()> {
        let line = format!("{}\r", command.trim_end_matches(['\r', '\n']));
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("the console writer lock was poisoned"))?;
        writer.write_all(line.as_bytes())?;
        writer.flush()
    }

    /// Has the child exited? Does not block.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Terminate the child immediately.
    ///
    /// This is the escalation, never the first move. The engine installs no
    /// SIGTERM handler and its shutdown does nine things including saving
    /// convars and logging the server off Steam's master list, all of which a
    /// kill skips.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }
}

/// Spawn a command on a fresh pty and start reading it.
///
/// Returns the child and a channel of its output. The reader is a blocking
/// thread rather than async: the pty master has no portable async read, and one
/// thread per server is not a cost worth engineering around.
pub fn spawn(
    command: &Command,
    working_dir: Option<&PathBuf>,
    env: &[(String, String)],
    redacted_command_line: String,
) -> Result<(Child, mpsc::Receiver<Output>), SpawnError> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: PTY_ROWS,
            cols: PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| SpawnError::OpenPty(e.to_string()))?;

    let mut builder = CommandBuilder::new(&command.program);
    for arg in &command.args {
        builder.arg(arg);
    }
    if let Some(dir) = working_dir {
        builder.cwd(dir);
    }
    for (key, value) in env {
        builder.env(key, value);
    }

    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|source| SpawnError::Spawn {
            program: command.program.clone(),
            source: anyhow_shim::Error(source.to_string()),
        })?;

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| SpawnError::Attach(e.to_string()))?;

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| SpawnError::Attach(e.to_string()))?;

    // The slave handle must be dropped here. Holding it open keeps the terminal
    // alive after the child exits, so the reader never sees EOF and the
    // supervisor waits forever for output that is not coming.
    drop(pair.slave);

    let (tx, rx) = mpsc::channel(256);

    std::thread::Builder::new()
        .name("cellar-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx
                            .blocking_send(Output::Bytes(buffer[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = tx.blocking_send(Output::Eof);
        })
        .map_err(|e| SpawnError::Attach(e.to_string()))?;

    let pid = child.process_id();

    Ok((
        Child {
            child,
            _master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            pid,
            command_line: redacted_command_line,
        },
        rx,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn shell_command(script: &str) -> Command {
        if cfg!(windows) {
            Command {
                program: "cmd.exe".to_owned(),
                args: vec!["/C".to_owned(), script.to_owned()],
            }
        } else {
            Command {
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), script.to_owned()],
            }
        }
    }

    #[tokio::test]
    async fn a_child_on_a_pty_produces_output_and_then_eof() {
        let (mut child, mut rx) = spawn(
            &shell_command("echo hello-from-the-pty"),
            None,
            &[],
            "test".into(),
        )
        .unwrap();

        let mut seen = String::new();
        while let Some(output) = rx.recv().await {
            match output {
                Output::Bytes(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                Output::Eof => break,
            }
        }

        assert!(seen.contains("hello-from-the-pty"), "saw: {seen:?}");
        // Dropping the slave is what makes this terminate rather than hang.
        let _ = child.try_wait();
    }

    /// The property the whole design rests on: the child sees a terminal, not a
    /// pipe. If this ever fails, the console channel is gone and `quit`, `kick`
    /// and every `applejack_*` command with it.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_believes_its_output_is_a_terminal() {
        let (mut child, mut rx) = spawn(
            &shell_command("if [ -t 1 ]; then echo IS-A-TTY; else echo IS-A-PIPE; fi"),
            None,
            &[],
            "test".into(),
        )
        .unwrap();

        let mut seen = String::new();
        while let Some(output) = rx.recv().await {
            match output {
                Output::Bytes(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                Output::Eof => break,
            }
        }

        assert!(seen.contains("IS-A-TTY"), "saw: {seen:?}");
        assert!(!seen.contains("IS-A-PIPE"));
        let _ = child.try_wait();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_typed_into_the_console_is_read_by_the_child() {
        // `read` from stdin is the same shape as the engine's console loop:
        // nothing arrives until a line is terminated.
        let (child, mut rx) = spawn(
            &shell_command("read line; echo GOT:$line"),
            None,
            &[],
            "test".into(),
        )
        .unwrap();

        child.send_command("applejack_features").unwrap();

        let mut seen = String::new();
        while let Some(output) = rx.recv().await {
            match output {
                Output::Bytes(bytes) => {
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                    if seen.contains("GOT:") {
                        break;
                    }
                }
                Output::Eof => break,
            }
        }

        assert!(seen.contains("GOT:applejack_features"), "saw: {seen:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_killed_child_still_reaches_eof() {
        let (mut child, mut rx) =
            spawn(&shell_command("sleep 30"), None, &[], "test".into()).unwrap();
        child.kill().unwrap();

        let mut saw_eof = false;
        while let Some(output) = rx.recv().await {
            if matches!(output, Output::Eof) {
                saw_eof = true;
                break;
            }
        }
        assert!(saw_eof);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_environment_reaches_the_child() {
        let (_child, mut rx) = spawn(
            &shell_command("echo ENV:$CELLAR_TEST_VALUE"),
            None,
            &[("CELLAR_TEST_VALUE".to_owned(), "present".to_owned())],
            "test".into(),
        )
        .unwrap();

        let mut seen = String::new();
        while let Some(output) = rx.recv().await {
            match output {
                Output::Bytes(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                Output::Eof => break,
            }
        }
        assert!(seen.contains("ENV:present"), "saw: {seen:?}");
    }
}

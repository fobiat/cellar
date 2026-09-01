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

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
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

/// How long Cellar stays alive after the sweep, so the HTTP reply reaches the
/// browser that asked for this before its connection dies with the process.
const ROOT_KILL_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

/// Kill every process Cellar owns, then Cellar itself, with no graceful stop.
///
/// This is deliberately not the supervisor's stop. It is for an operator who
/// needs the whole local stack gone now, including a server that has stopped
/// answering its console.
pub fn emergency_kill_current_process_tree() {
    let root = Pid::from_u32(std::process::id());
    kill_descendants(root, &process_table());

    // After the sweep, never before: the helper is itself a child of Cellar, so
    // a sweep run later would kill the thing doing the killing.
    schedule_root_kill(root.as_u32());
}

/// Every process on the machine, carrying only the fields the walk reads.
///
/// `System::new_all` also collects command lines, environments, memory and disk
/// usage for all of them, which is a great deal of work to then throw away.
fn process_table() -> System {
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::new());
    system
}

/// SIGKILL, or `TerminateProcess` on Windows, to everything under `root`.
///
/// Deepest first, so a parent cannot notice a dead child and restart it while
/// the walk is still going.
fn kill_descendants(root: Pid, system: &System) {
    for pid in descendants_in_postorder(root, &children_by_parent(system)) {
        if let Some(process) = system.process(pid)
            && !process.kill()
        {
            tracing::warn!(pid = pid.as_u32(), "could not kill a process Cellar owns");
        }
    }
}

fn children_by_parent(system: &System) -> HashMap<Pid, Vec<Pid>> {
    let mut children = HashMap::<Pid, Vec<Pid>>::new();
    for (pid, process) in system.processes() {
        let Some(parent) = process.parent() else {
            continue;
        };
        // Windows leaves a dead parent's id on its orphans and reuses pids, so
        // an unrelated process can claim Cellar as its parent. A real child
        // cannot have started before the parent did.
        if system
            .process(parent)
            .is_some_and(|owner| owner.start_time() > process.start_time())
        {
            continue;
        }
        children.entry(parent).or_default().push(*pid);
    }
    children
}

fn descendants_in_postorder(root: Pid, children: &HashMap<Pid, Vec<Pid>>) -> Vec<Pid> {
    fn visit(
        pid: Pid,
        children: &HashMap<Pid, Vec<Pid>>,
        seen: &mut HashSet<Pid>,
        result: &mut Vec<Pid>,
    ) {
        if !seen.insert(pid) {
            return;
        }

        if let Some(kids) = children.get(&pid) {
            for child in kids {
                visit(*child, children, seen, result);
            }
        }
        result.push(pid);
    }

    let mut result = Vec::new();
    visit(root, children, &mut HashSet::new(), &mut result);
    result.pop();
    result
}

/// Arrange for Cellar itself to be gone shortly, two ways.
///
/// The thread is what normally does it. The detached helper is the fallback for
/// the cases the thread cannot cover: a wedged process that never reaches the
/// exit, and any atexit handler that decides to block on the way out.
fn schedule_root_kill(pid: u32) {
    // A plain OS thread, not a task. The async runtime is one of the things
    // being torn down here.
    std::thread::spawn(|| {
        std::thread::sleep(ROOT_KILL_DELAY);
        std::process::exit(EMERGENCY_KILL_EXIT_CODE);
    });

    if let Err(why) = spawn_root_killer(pid) {
        tracing::warn!("could not spawn the fallback killer for Cellar itself: {why}");
    }
}

/// What Cellar exits with when an operator kills it from the dashboard, so a
/// service manager's log says which of the two shutdowns this was.
pub const EMERGENCY_KILL_EXIT_CODE: i32 = 137;

fn spawn_root_killer(pid: u32) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // No `/T`. The tree it would walk contains this helper and the
        // taskkill running inside it, so which of the three dies first would be
        // down to enumeration order. The sweep already took the descendants.
        let script = format!("Start-Sleep -Milliseconds 800; taskkill.exe /PID {pid} /F");
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                script.as_str(),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    }

    #[cfg(unix)]
    {
        // Whole seconds: fractional `sleep` is a GNU and BSD extension, not
        // something POSIX `sh` owes anyone.
        let script = format!("sleep 1; kill -KILL {pid}");
        std::process::Command::new("sh")
            .args(["-c", script.as_str()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    }

    Ok(())
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

    #[test]
    fn descendants_are_returned_deepest_first_without_the_root() {
        let root = Pid::from_u32(1);
        let child = Pid::from_u32(2);
        let grandchild = Pid::from_u32(3);
        let mut children = HashMap::new();
        children.insert(root, vec![child]);
        children.insert(child, vec![grandchild]);

        assert_eq!(
            descendants_in_postorder(root, &children),
            vec![grandchild, child]
        );
    }

    /// The sweep against a real process table, on a throwaway tree this test
    /// built. It starts at a shell spawned here and not at Cellar, so nothing
    /// else on the machine is ever in scope.
    #[test]
    fn the_sweep_reaches_a_grandchild_it_did_not_spawn_itself() {
        let (mut parent, grandchild_name) = if cfg!(windows) {
            // `ping` counts seconds and needs no console input, which `pause`
            // and `timeout` both do.
            let child = std::process::Command::new("cmd.exe")
                .args(["/C", "ping", "-n", "120", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            (child, "ping")
        } else {
            // `sh -c` execs a lone command instead of forking one, so it needs
            // something to wait on before there is a grandchild at all.
            let child = std::process::Command::new("/bin/sh")
                .args(["-c", "sleep 120 & wait"])
                .spawn()
                .unwrap();
            (child, "sleep")
        };
        let parent_pid = Pid::from_u32(parent.id());

        let found = wait_for(|| {
            let system = process_table();
            let grandchild = children_by_parent(&system)
                .get(&parent_pid)?
                .iter()
                .find(|pid| {
                    // Lowercased: cmd.exe reports its child as `PING.EXE`.
                    system.process(**pid).is_some_and(|kid| {
                        kid.name()
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(grandchild_name)
                    })
                })
                .copied()?;
            Some((system, grandchild))
        });
        let Some((system, grandchild)) = found else {
            let _ = parent.kill();
            let _ = parent.wait();
            panic!("the throwaway shell started a child of its own");
        };

        kill_descendants(parent_pid, &system);
        let gone = wait_for(|| process_table().process(grandchild).is_none().then_some(()));

        let _ = parent.kill();
        let _ = parent.wait();
        if gone.is_none()
            && let Some(survivor) = process_table().process(grandchild)
        {
            survivor.kill();
        }

        assert!(gone.is_some(), "the grandchild outlived the sweep");
    }

    /// Poll until `check` answers, for up to twenty seconds. Both platforms take
    /// their own time to show a new process and to retire a dead one.
    fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(answer) = check() {
                return Some(answer);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

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

    /// Every wait here is bounded. A pty that never speaks is a plausible
    /// failure, and an unbounded `recv` turns it into a suite that hangs
    /// forever rather than a test that fails.
    const PTY_TEST_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

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
        let found = tokio::time::timeout(PTY_TEST_LIMIT, async {
            while let Some(output) = rx.recv().await {
                match output {
                    Output::Bytes(bytes) => {
                        seen.push_str(&String::from_utf8_lossy(&bytes));
                        if seen.contains("hello-from-the-pty") {
                            return true;
                        }
                    }
                    Output::Eof => return false,
                }
            }
            false
        })
        .await;

        assert_eq!(found, Ok(true), "saw: {seen:?}");

        // Unix only, measured rather than assumed: ConPTY holds the master
        // readable after the child exits, so the read that reports EOF on a
        // unix pty simply never returns on Windows. Waiting for one there hung
        // the whole suite. The supervisor does not depend on it either way; it
        // decides a run is over from `try_wait`.
        #[cfg(unix)]
        {
            let eof = tokio::time::timeout(PTY_TEST_LIMIT, async {
                while let Some(output) = rx.recv().await {
                    if matches!(output, Output::Eof) {
                        return true;
                    }
                }
                false
            })
            .await;
            assert_eq!(eof, Ok(true), "the pty never reported EOF");
        }

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

//! The loop that owns the server process.
//!
//! One task owns the child, the terminal, the log tailer, the sampler and the
//! restart policy, and publishes an event stream. Everything else in Cellar (the
//! CLI, the TUI, the web UI, the webhooks) is a consumer of that stream and a
//! sender on the control channel, which is what keeps three interfaces from
//! disagreeing about what the server is doing.

use std::time::{Duration, Instant};

use cellar_core::ansi::LineAssembler;
use cellar_core::config::Config;
use cellar_core::event::{Event, Origin, StatusBar};
use cellar_core::grammar::{self, Line};
use cellar_core::lifecycle::{Decision, RestartTracker, State};
use cellar_core::snapshot::{Snapshot, Tracker};
use cellar_core::statusbar;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::logfile::Tailer;
use crate::metrics::Sampler;
use crate::process::{self, Child, Output};
use crate::{hosting, launch};

/// Backstop for a command whose reply is never bracketed.
///
/// The console brackets a reply exactly (see [`PendingReply`]), so this fires
/// only when the child has no usable terminal and the markers never arrive. It
/// is longer than the old blind window because it is no longer the mechanism,
/// just the way a degraded console still answers instead of hanging.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the console is held before it is allowed to be the event source.
///
/// Long enough for the log-file tailer, which polls, to get a turn. See the
/// reasoning where it is used in `run_once`.
const CONSOLE_GRACE: Duration = Duration::from_secs(2);

/// A command awaiting its reply.
///
/// The engine brackets console output precisely, which is worth stating because
/// the obvious reading is that it does not. `ConsoleInput.OnEnter` writes
/// `"> " + inputString` *before* calling `OnInputText`, and `RedrawInputLine`
/// repaints the status block *after* `ConVarSystem.Run` returns. A reply is
/// therefore exactly the lines between the echo and the next status line, with
/// no timing guess involved.
///
/// Those markers exist only on the pty, never in the log file, so `bracketed` is
/// filled from the console whatever channel is otherwise authoritative. That
/// also means a reply is never double-counted: the log file supplies events, the
/// console supplies replies.
struct PendingReply {
    command: String,
    /// Lines between the `> {command}` echo and the status redraw.
    bracketed: Vec<String>,
    /// Everything seen since dispatch, used only if the echo never arrives.
    fallback: Vec<String>,
    /// Set by the echo, cleared by sending. While false, arriving console lines
    /// belong to whatever the server was already saying.
    open: bool,
    started: Instant,
    reply: oneshot::Sender<Result<Vec<String>, String>>,
}

impl PendingReply {
    /// Answer the caller with the bracketed reply, or the unbracketed fallback
    /// when the console never echoed.
    fn complete(self) -> (String, Vec<String>) {
        let lines = if self.open || !self.bracketed.is_empty() {
            self.bracketed
        } else {
            self.fallback
        };
        let _ = self.reply.send(Ok(lines.clone()));
        (self.command, lines)
    }
}

/// What an outside caller can ask the supervisor to do.
#[derive(Debug)]
pub enum Control {
    /// Type a command into the server console and collect what follows.
    Exec {
        command: String,
        actor: String,
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    /// Graceful stop: `quit`, then wait, then kill.
    Stop { reply: oneshot::Sender<()> },
    /// Stop and start again, resetting the backoff.
    Restart { reply: oneshot::Sender<()> },
    /// Read the current state.
    Snapshot { reply: oneshot::Sender<Snapshot> },
}

/// Handle for talking to a running supervisor.
#[derive(Clone)]
pub struct Handle {
    control: mpsc::Sender<Control>,
    events: broadcast::Sender<Event>,
}

impl Handle {
    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn snapshot(&self) -> Option<Snapshot> {
        let (reply, rx) = oneshot::channel();
        self.control.send(Control::Snapshot { reply }).await.ok()?;
        rx.await.ok()
    }

    pub async fn exec(&self, command: &str, actor: &str) -> Result<Vec<String>, String> {
        let (reply, rx) = oneshot::channel();
        self.control
            .send(Control::Exec {
                command: command.to_owned(),
                actor: actor.to_owned(),
                reply,
            })
            .await
            .map_err(|_| "the supervisor is not running".to_owned())?;

        rx.await
            .map_err(|_| "the supervisor stopped before replying".to_owned())?
    }

    pub async fn stop(&self) {
        let (reply, rx) = oneshot::channel();
        if self.control.send(Control::Stop { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }

    pub async fn restart(&self) {
        let (reply, rx) = oneshot::channel();
        if self.control.send(Control::Restart { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }
}

/// Owns the child process and everything watching it.
pub struct Supervisor {
    config: Config,
    tracker: Tracker,
    restarts: RestartTracker,
    sampler: Sampler,
    events: broadcast::Sender<Event>,
    started: Instant,
    /// The two halves of the status bar, merged as they arrive. The engine draws
    /// them as separate lines, so neither is ever a whole bar on its own.
    status: StatusBar,
}

impl Supervisor {
    /// Build a supervisor and the handle for talking to it.
    pub fn new(config: Config) -> (Self, Handle, mpsc::Receiver<Control>) {
        let (events, _) = broadcast::channel(1024);
        let (control_tx, control_rx) = mpsc::channel(64);

        let tracker = Tracker::new(config.server.hostname.clone(), 0);

        let handle = Handle {
            control: control_tx,
            events: events.clone(),
        };

        (
            Self {
                config,
                tracker,
                restarts: RestartTracker::new(),
                sampler: Sampler::new(),
                events,
                started: Instant::now(),
                status: StatusBar::default(),
            },
            handle,
            control_rx,
        )
    }

    fn publish(&mut self, event: Event) {
        self.tracker.apply(&event, chrono::Utc::now());
        // A send fails only when nothing is subscribed, which is normal.
        let _ = self.events.send(event);
    }

    /// Write `hosting.json` so the gamemode dials the bridge Cellar is binding.
    ///
    /// Returns the message to log, or the reason it could not be written. Not
    /// fatal: a server without a bridge is a server on local files, which is the
    /// gamemode's own default.
    pub fn prepare_hosting(&self) -> Result<String, String> {
        let document = hosting::document_for(&self.config.bridge);

        let Some(path) = hosting::document_path(&self.config.server) else {
            if self.config.bridge.enabled {
                return Err(
                    "bridge.enabled but server.data_dir is unset, so hosting.json cannot be \
                     written and the gamemode will keep using local files. Point data_dir at the \
                     directory holding features.json."
                        .to_owned(),
                );
            }
            return Ok("no data_dir set; leaving the gamemode's storage provider alone".to_owned());
        };

        hosting::write(&path, &document)
            .map(|()| format!("wrote {} selecting '{}'", path.display(), document.provider))
            .map_err(|e| format!("could not write {}: {e}", path.display()))
    }

    /// Run until told to stop, or until the restart policy gives up.
    pub async fn run(mut self, mut control: mpsc::Receiver<Control>) {
        loop {
            match self.run_once(&mut control).await {
                RunOutcome::Stopped => {
                    self.tracker.set_state(State::Stopped);
                    return;
                }
                RunOutcome::GaveUp => {
                    self.tracker.set_state(State::CrashLooping);
                    return;
                }
                RunOutcome::RestartAfter(delay) => {
                    self.tracker.set_state(State::Backoff);
                    self.tracker
                        .set_consecutive_failures(self.restarts.consecutive_failures());

                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        message = control.recv() => {
                            match message {
                                Some(Control::Stop { reply }) => {
                                    let _ = reply.send(());
                                    self.tracker.set_state(State::Stopped);
                                    return;
                                }
                                Some(Control::Snapshot { reply }) => {
                                    let _ = reply.send(self.tracker.snapshot());
                                }
                                // A restart during backoff means "stop waiting".
                                Some(Control::Restart { reply }) => {
                                    self.restarts.record_healthy_run();
                                    let _ = reply.send(());
                                }
                                Some(Control::Exec { reply, .. }) => {
                                    let _ = reply.send(Err("the server is not running".to_owned()));
                                }
                                None => return,
                            }
                        }
                    }
                }
            }
        }
    }

    async fn run_once(&mut self, control: &mut mpsc::Receiver<Control>) -> RunOutcome {
        let needs_local_http = self.config.bridge.enabled;
        let command = launch::command_for(&self.config.server, needs_local_http);
        let redacted = command.redacted(self.config.server.gslt.as_ref());

        // Armed before the child starts, so the follow position is this run's
        // first byte. Arming after would race the engine's boot lines, and the
        // engine writes several before anything else happens.
        let mut tailer = Tailer::new(launch::log_file_for(&self.config.server));
        tailer.poll();

        let spawned = process::spawn(
            &command,
            self.config.server.working_dir.as_ref(),
            &[],
            redacted.clone(),
        );

        let (mut child, mut output) = match spawned {
            Ok(pair) => pair,
            Err(error) => {
                self.publish(Event::Unparsed {
                    raw: format!("could not start the server: {error}"),
                    origin: Origin::Cellar,
                });
                let now = self.started.elapsed().as_secs();
                return match self.restarts.on_exit(
                    None,
                    false,
                    Duration::ZERO,
                    now,
                    self.config.supervisor.restart,
                    self.config.supervisor.backoff,
                ) {
                    Decision::RestartAfter(delay) => RunOutcome::RestartAfter(delay),
                    Decision::Stop => RunOutcome::Stopped,
                    Decision::GiveUp => RunOutcome::GaveUp,
                };
            }
        };

        let pid = child.pid().unwrap_or(0);
        self.publish(Event::ProcessStarted {
            pid,
            command: redacted,
        });

        let mut assembler = LineAssembler::new();
        let ready_pattern = self.config.server.ready_pattern.clone();

        // Only one channel produces events, or every line is counted twice.
        //
        // The log file is preferred: it keeps the whole logger name where the
        // console truncates to eight characters, and it carries a date. But it
        // is polled, and the console is not, so a naive "whichever speaks first"
        // races and emits the boot lines from both.
        //
        // So the console is held for a moment. Its lines are buffered rather
        // than dropped, and if the log file has still said nothing when the
        // grace expires, the buffer is replayed and the console becomes the
        // source for the rest of the run. A server whose log file Cellar cannot
        // read is then still followed, just with less fidelity, rather than
        // silently unmonitored.
        let mut log_file_speaking = false;
        let mut console_authoritative = false;
        let mut console_backlog: Vec<String> = Vec::new();

        let mut tail_tick = tokio::time::interval(Duration::from_millis(250));
        let mut sample_tick = tokio::time::interval(Duration::from_secs(
            self.config.supervisor.sample_interval_seconds.max(1),
        ));
        let mut exit_tick = tokio::time::interval(Duration::from_millis(250));

        let run_started = Instant::now();
        let mut requested_stop = false;
        let mut restart_requested = false;
        // The command awaiting its reply, if any.
        let mut collecting: Option<PendingReply> = None;

        let exit_code = loop {
            tokio::select! {
                Some(chunk) = output.recv() => {
                    let lines = match chunk {
                        Output::Bytes(bytes) => assembler.push(&bytes),
                        Output::Eof => assembler.flush().into_iter().collect(),
                    };

                    for line in lines {
                        // Always ingested with `emit` false first, because the
                        // status bar is read from the console whatever else is.
                        self.ingest(&line, Origin::Console, &ready_pattern, &mut collecting, console_authoritative);

                        if !console_authoritative && !log_file_speaking {
                            console_backlog.push(line);
                        }
                    }
                }

                _ = tail_tick.tick() => {
                    let lines = tailer.poll();
                    if !lines.is_empty() {
                        log_file_speaking = true;
                        // The log file has these, and better. Nothing is lost.
                        console_backlog.clear();
                    }
                    for line in lines {
                        self.ingest(&line, Origin::LogFile, &ready_pattern, &mut collecting, true);
                    }

                    if !log_file_speaking
                        && !console_authoritative
                        && run_started.elapsed() >= CONSOLE_GRACE
                    {
                        console_authoritative = true;
                        for line in std::mem::take(&mut console_backlog) {
                            self.ingest(&line, Origin::Console, &ready_pattern, &mut collecting, true);
                        }
                    }

                    // Backstop only: a bracketed reply has already been sent by
                    // the status line that terminated it.
                    if let Some(p) = &collecting
                        && p.started.elapsed() >= REPLY_TIMEOUT
                        && let Some(p) = collecting.take()
                    {
                        let (command, reply) = p.complete();
                        self.publish(Event::CommandReplied { command, reply, ok: true });
                    }
                }

                _ = sample_tick.tick() => {
                    if pid != 0 && let Some(sample) = self.sampler.sample(pid) {
                        self.publish(Event::Resources(sample));
                    }
                }

                _ = exit_tick.tick() => {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status.exit_code() as i32),
                        Ok(None) => {}
                        Err(_) => break None,
                    }
                }

                message = control.recv() => {
                    match message {
                        Some(Control::Snapshot { reply }) => {
                            let _ = reply.send(self.tracker.snapshot());
                        }
                        Some(Control::Exec { command, actor, reply }) => {
                            match child.send_command(&command) {
                                Ok(()) => {
                                    self.publish(Event::CommandDispatched {
                                        command: command.clone(),
                                        actor,
                                    });
                                    // Replace any in-flight collection; the newer
                                    // command is the one the caller is waiting on.
                                    if let Some(previous) = collecting.take() {
                                        let (old, lines) = previous.complete();
                                        self.publish(Event::CommandReplied {
                                            command: old,
                                            reply: lines,
                                            ok: true,
                                        });
                                    }
                                    collecting = Some(PendingReply {
                                        command,
                                        bracketed: Vec::new(),
                                        fallback: Vec::new(),
                                        open: false,
                                        started: Instant::now(),
                                        reply,
                                    });
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(format!("could not reach the console: {error}")));
                                }
                            }
                        }
                        Some(Control::Stop { reply }) => {
                            requested_stop = true;
                            let status = self.graceful_stop(&mut child, &mut output, &mut assembler).await;
                            let _ = reply.send(());
                            break status;
                        }
                        Some(Control::Restart { reply }) => {
                            restart_requested = true;
                            let status = self.graceful_stop(&mut child, &mut output, &mut assembler).await;
                            let _ = reply.send(());
                            break status;
                        }
                        None => {
                            requested_stop = true;
                            break self.graceful_stop(&mut child, &mut output, &mut assembler).await;
                        }
                    }
                }
            }
        };

        // Anything the log wrote in its last moments is worth having.
        for line in tailer.poll() {
            self.ingest(
                &line,
                Origin::LogFile,
                &ready_pattern,
                &mut collecting,
                true,
            );
        }
        if let Some(last) = tailer.flush() {
            self.ingest(
                &last,
                Origin::LogFile,
                &ready_pattern,
                &mut collecting,
                true,
            );
        }
        if let Some(pending) = collecting.take() {
            let (command, reply) = pending.complete();
            self.publish(Event::CommandReplied {
                command,
                reply,
                ok: true,
            });
        }

        self.publish(Event::ProcessExited {
            code: exit_code,
            graceful: requested_stop || restart_requested,
        });

        if restart_requested {
            self.restarts.record_healthy_run();
            return RunOutcome::RestartAfter(Duration::from_millis(500));
        }

        match self.restarts.on_exit(
            exit_code,
            requested_stop,
            run_started.elapsed(),
            self.started.elapsed().as_secs(),
            self.config.supervisor.restart,
            self.config.supervisor.backoff,
        ) {
            Decision::RestartAfter(delay) => RunOutcome::RestartAfter(delay),
            Decision::Stop => RunOutcome::Stopped,
            Decision::GiveUp => RunOutcome::GaveUp,
        }
    }

    /// `quit`, then wait, then kill.
    ///
    /// The engine installs no SIGTERM or Ctrl+C handler anywhere, and its clean
    /// shutdown does nine things including saving every `Saved` convar and
    /// logging the server off Steam's master list. A kill skips all of it, which
    /// is what every Kubernetes rollout does to this server today.
    /// Returns the exit status it observed, if any.
    ///
    /// Returning it matters: `try_wait` reports a status once and answers `None`
    /// afterwards, so a caller that waits here and then asks again gets nothing
    /// and reports a clean shutdown as "killed by a signal".
    async fn graceful_stop(
        &mut self,
        child: &mut Child,
        output: &mut mpsc::Receiver<Output>,
        assembler: &mut LineAssembler,
    ) -> Option<i32> {
        self.tracker.set_state(State::Stopping);

        if child.send_command("quit").is_err() {
            let _ = child.kill();
            return None;
        }

        let deadline = Duration::from_secs(self.config.supervisor.graceful_timeout_seconds.max(1));
        let waited = tokio::time::timeout(deadline, async {
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return Some(status.exit_code() as i32),
                    Err(_) => return None,
                    Ok(None) => {}
                }

                // Keep draining while it shuts down; the shutdown log is exactly
                // what tells an operator the Steam logoff completed.
                match tokio::time::timeout(Duration::from_millis(100), output.recv()).await {
                    Ok(Some(Output::Bytes(bytes))) => {
                        for line in assembler.push(&bytes) {
                            let _ = self.events.send(Event::Unparsed {
                                raw: line,
                                origin: Origin::Console,
                            });
                        }
                    }
                    // The terminal closed. The child is on its way out; one more
                    // check picks up the status rather than reporting none.
                    Ok(Some(Output::Eof)) | Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        return child
                            .try_wait()
                            .ok()
                            .flatten()
                            .map(|s| s.exit_code() as i32);
                    }
                    Err(_) => {}
                }
            }
        })
        .await;

        match waited {
            Ok(status) => status,
            Err(_) => {
                let _ = self.events.send(Event::Unparsed {
                    raw: format!(
                        "the server did not exit within {}s of `quit`; killing it, which skips the \
                         Steam logoff and the convar save",
                        deadline.as_secs()
                    ),
                    origin: Origin::Cellar,
                });
                let _ = child.kill();
                None
            }
        }
    }

    /// Turn one raw line into events.
    ///
    /// `emit` is false for the console once the log file is speaking: the status
    /// bar is still read from it, because that is the only place the engine
    /// reports its frame timings, but nothing else is, or every line would be
    /// counted twice.
    fn ingest(
        &mut self,
        raw: &str,
        origin: Origin,
        ready_pattern: &str,
        pending: &mut Option<PendingReply>,
        emit: bool,
    ) {
        let from_console = origin != Origin::LogFile;
        let line = if from_console {
            Line::console(raw)
        } else {
            Line::log_file(raw)
        };

        let Some(parsed) = grammar::parse_line(line) else {
            return;
        };

        if from_console {
            // The status bar is drawn straight to the terminal and never
            // reaches the log file, so it is read from the console only. It is
            // also the terminator for a command's reply.
            if let Some(fragment) = grammar::parse_status_fragment(&parsed.message) {
                fragment.apply(&mut self.status);
                let bar = self.status.clone();
                self.publish(Event::Status(bar));

                if let Some(p) = pending.take() {
                    if p.open {
                        let (command, reply) = p.complete();
                        self.publish(Event::CommandReplied {
                            command,
                            reply,
                            ok: true,
                        });
                    } else {
                        *pending = Some(p);
                    }
                }
                return;
            }

            // `> {command}`, written before the ConCmd runs. Not output.
            if let Some(echoed) = statusbar::parse_command_echo(&parsed.message) {
                if let Some(p) = pending.as_mut()
                    && echoed.trim() == p.command.trim()
                {
                    p.open = true;
                }
                return;
            }

            if let Some(p) = pending.as_mut()
                && !statusbar::is_blank_chrome(&parsed.message)
            {
                if p.open {
                    p.bracketed.push(parsed.message.clone());
                } else {
                    p.fallback.push(parsed.message.clone());
                }
            }
        }

        if !emit {
            return;
        }

        let event = grammar::classify(&parsed, origin, ready_pattern);
        self.publish(event);
    }
}

enum RunOutcome {
    Stopped,
    GaveUp,
    RestartAfter(Duration),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use cellar_core::config::{Launcher, ServerConfig};

    use super::*;

    fn config(data_dir: Option<PathBuf>) -> Config {
        Config {
            server: ServerConfig {
                executable: PathBuf::from("/bin/true"),
                project: PathBuf::from("/tmp/a.sbproj"),
                game: None,
                map: None,
                launcher: Launcher::Native,
                working_dir: None,
                log_file: None,
                hostname: "test".into(),
                gslt: None,
                direct_connect: false,
                port: 27015,
                query_port: 27016,
                ready_pattern: "Lobby created".into(),
                extra_args: Vec::new(),
                data_dir,
            },
            supervisor: Default::default(),
            bridge: Default::default(),
            database: Default::default(),
            web: Default::default(),
            notify: Default::default(),
            update: Default::default(),
            mariadb: Default::default(),
        }
    }

    #[test]
    fn hosting_json_is_written_next_to_the_other_documents() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(Some(dir.path().to_path_buf()));
        config.bridge.enabled = true;
        config.bridge.public_url = "http://127.0.0.1:8080".into();

        let (supervisor, _handle, _control) = Supervisor::new(config);
        let message = supervisor.prepare_hosting().unwrap();
        assert!(message.contains("hosted"), "{message}");

        let written = std::fs::read_to_string(dir.path().join("hosting.json")).unwrap();
        assert!(written.contains("\"bridgeUrl\": \"http://127.0.0.1:8080\""));
    }

    /// A bridge with nowhere to write `hosting.json` would silently leave the
    /// gamemode on local files, which is the quiet failure worth refusing.
    #[test]
    fn a_bridge_without_a_data_dir_says_so_instead_of_going_quiet() {
        let mut config = config(None);
        config.bridge.enabled = true;

        let (supervisor, _handle, _control) = Supervisor::new(config);
        let error = supervisor.prepare_hosting().unwrap_err();
        assert!(error.contains("data_dir"), "{error}");
    }

    #[test]
    fn no_bridge_and_no_data_dir_is_simply_fine() {
        let (supervisor, _handle, _control) = Supervisor::new(config(None));
        assert!(supervisor.prepare_hosting().is_ok());
    }
}

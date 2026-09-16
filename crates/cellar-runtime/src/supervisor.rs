//! The loop that owns the server process.
//!
//! One task owns the child, the terminal, the log tailer, the sampler and the
//! restart policy, and publishes an event stream. Everything else in Cellar (the
//! CLI, the TUI, the web UI, the webhooks) is a consumer of that stream and a
//! sender on the control channel, which is what keeps three interfaces from
//! disagreeing about what the server is doing.

use std::time::{Duration, Instant};

use cellar_core::ansi::LineAssembler;
use cellar_core::config::{Instance, Launcher, ServerConfig};
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

const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const KILL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);

trait TerminationChild {
    fn send_command(&self, command: &str) -> std::io::Result<()>;
    fn try_wait_code(&mut self) -> std::io::Result<Option<i32>>;
    fn kill(&mut self) -> std::io::Result<()>;
}

impl TerminationChild for Child {
    fn send_command(&self, command: &str) -> std::io::Result<()> {
        Self::send_command(self, command)
    }

    fn try_wait_code(&mut self) -> std::io::Result<Option<i32>> {
        self.try_wait()
            .map(|status| status.map(|status| status.exit_code() as i32))
    }

    fn kill(&mut self) -> std::io::Result<()> {
        Self::kill(self)
    }
}

async fn wait_for_exit(
    child: &mut impl TerminationChild,
    timeout: Duration,
) -> Result<i32, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait_code() {
            Ok(Some(code)) => return Ok(code),
            Ok(None) => {}
            Err(error) => return Err(format!("could not confirm that the server exited: {error}")),
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "the server did not exit within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(remaining.min(EXIT_POLL_INTERVAL)).await;
    }
}

async fn kill_and_confirm(
    child: &mut impl TerminationChild,
    timeout: Duration,
) -> Result<i32, String> {
    if let Err(error) = child.kill() {
        return match child.try_wait_code() {
            Ok(Some(code)) => Ok(code),
            Ok(None) => Err(format!(
                "could not kill the server and it is still running: {error}"
            )),
            Err(wait_error) => Err(format!(
                "could not kill the server ({error}) or confirm that it exited ({wait_error})"
            )),
        };
    }

    wait_for_exit(child, timeout)
        .await
        .map_err(|error| format!("the server was killed but exit was not confirmed: {error}"))
}

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
    Stop {
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Stop and start again, resetting the backoff.
    Restart {
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// End the supervisor task itself.
    ///
    /// Distinct from [`Control::Stop`], which stops the game server and leaves
    /// the supervisor answering. Cellar's own exit path is the only caller.
    Shutdown {
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Replace the server profile and restart it without rebinding Cellar.
    SwitchConfig {
        instance: Box<Instance>,
        reply: oneshot::Sender<Result<(), String>>,
    },
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

    pub async fn stop(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.control
            .send(Control::Stop { reply })
            .await
            .map_err(|_| "the supervisor is not running".to_owned())?;
        rx.await
            .map_err(|_| "the supervisor stopped before confirming the server exit".to_owned())?
    }

    pub async fn restart(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.control
            .send(Control::Restart { reply })
            .await
            .map_err(|_| "the supervisor is not running".to_owned())?;
        rx.await
            .map_err(|_| "the supervisor stopped before confirming the server exit".to_owned())?
    }

    /// End the supervisor task. The server should be stopped first.
    pub async fn shutdown(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.control
            .send(Control::Shutdown { reply })
            .await
            .map_err(|_| "the supervisor is not running".to_owned())?;
        rx.await
            .map_err(|_| "the supervisor stopped before confirming the server exit".to_owned())?
    }

    pub async fn switch_config(&self, instance: Instance) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.control
            .send(Control::SwitchConfig {
                instance: Box::new(instance),
                reply,
            })
            .await
            .map_err(|_| "the supervisor is not running".to_owned())?;
        rx.await
            .map_err(|_| "the supervisor stopped before switching config".to_owned())?
    }
}

/// Owns the child process and everything watching it.
pub struct Supervisor {
    /// The one instance this supervisor owns, with every default resolved.
    ///
    /// Not the whole `Config`: a supervisor has no business reading the web
    /// binding or the backup schedule, and holding the process-wide config was
    /// how it came to read the primary's server settings whichever instance it
    /// was actually running.
    instance: Instance,
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
    pub fn new(instance: Instance) -> (Self, Handle, mpsc::Receiver<Control>) {
        let (events, _) = broadcast::channel(1024);
        let (control_tx, control_rx) = mpsc::channel(64);

        // Zero when nobody has read the project's `Metadata.MaxPlayers`, which
        // is the only place the real ceiling exists. Zero means unknown, and
        // the dashboard says so rather than showing "0/0".
        let tracker = Tracker::new(
            instance.server.hostname.clone(),
            instance.player_ceiling.unwrap_or(0),
        );

        let handle = Handle {
            control: control_tx,
            events: events.clone(),
        };

        (
            Self {
                instance,
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

    /// Adopt a new profile, rolling back if it cannot be prepared.
    ///
    /// One place, because three call sites each rolling back by hand is three
    /// chances for a half-applied switch.
    fn switch_to(&mut self, instance: Instance) -> Result<(), String> {
        let previous = std::mem::replace(&mut self.instance, instance);

        match self.prepare_hosting() {
            Ok(_) => Ok(()),
            Err(why) => {
                self.instance = previous;
                Err(why)
            }
        }
    }

    fn report_stop_failure(&mut self, error: String) {
        tracing::error!("server termination was not confirmed: {error}");
        self.tracker.set_state(State::StopFailed);
        self.publish(Event::Unparsed {
            raw: format!("server termination was not confirmed: {error}"),
            origin: Origin::Cellar,
        });
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
        let document = hosting::document_for(&self.instance.bridge);

        let Some(path) = hosting::document_path(&self.instance.server) else {
            if self.instance.bridge.enabled {
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

    /// Run until Cellar itself is shutting down.
    ///
    /// A stopped or crash-looping server does not end this task: it rests,
    /// still answering snapshots and still able to be restarted. Returning
    /// instead would take the roster, the resource history, the exit code and
    /// the restart button with it, and leave `/api/status` reporting an absence
    /// where an operator needs a reason.
    pub async fn run(mut self, mut control: mpsc::Receiver<Control>) {
        loop {
            match self.run_once(&mut control).await {
                RunOutcome::ShutDown => return,
                RunOutcome::Stopped => {
                    tracing::info!("the server is stopped and will not be restarted");
                    self.tracker.set_state(State::Stopped);
                    if self.rest(&mut control).await == Resting::Done {
                        return;
                    }
                }
                RunOutcome::GaveUp => {
                    tracing::error!(
                        "the server has restarted {} times inside the crash-loop window; giving \
                         up rather than hiding the fault and burning the Steam registration",
                        self.instance.supervisor.backoff.crash_loop_threshold,
                    );
                    self.tracker.set_state(State::CrashLooping);
                    if self.rest(&mut control).await == Resting::Done {
                        return;
                    }
                }
                RunOutcome::RestartAfter(delay) => {
                    tracing::info!(
                        "restarting the server in {:?} (consecutive failures: {})",
                        delay,
                        self.restarts.consecutive_failures(),
                    );
                    self.tracker.set_state(State::Backoff);
                    self.tracker
                        .set_consecutive_failures(self.restarts.consecutive_failures());

                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        message = control.recv() => {
                            match message {
                                Some(Control::Stop { reply }) => {
                                    let _ = reply.send(Ok(()));
                                    tracing::info!("the backoff wait was cancelled by a stop");
                                    self.tracker.set_state(State::Stopped);
                                    if self.rest(&mut control).await == Resting::Done {
                                        return;
                                    }
                                }
                                Some(Control::Shutdown { reply }) => {
                                    let _ = reply.send(Ok(()));
                                    return;
                                }
                                Some(Control::Snapshot { reply }) => {
                                    let _ = reply.send(self.tracker.snapshot());
                                }
                                // A restart during backoff means "stop waiting".
                                Some(Control::Restart { reply }) => {
                                    self.restarts.record_healthy_run();
                                    let _ = reply.send(Ok(()));
                                }
                                Some(Control::Exec { reply, .. }) => {
                                    let _ = reply.send(Err("the server is not running".to_owned()));
                                }
                                Some(Control::SwitchConfig { instance, reply }) => {
                                    let result = self.switch_to(*instance);
                                    let _ = reply.send(result);
                                }
                                None => return,
                            }
                        }
                    }
                }
            }
        }
    }

    /// Answer control messages with no server running.
    ///
    /// Returns [`Resting::Resume`] when asked to start the server again, and
    /// [`Resting::Done`] when Cellar is shutting down or the last handle has
    /// been dropped.
    async fn rest(&mut self, control: &mut mpsc::Receiver<Control>) -> Resting {
        loop {
            match control.recv().await {
                Some(Control::Snapshot { reply }) => {
                    let _ = reply.send(self.tracker.snapshot());
                }
                Some(Control::Restart { reply }) => {
                    tracing::info!("starting the server again on request");
                    self.restarts.record_healthy_run();
                    let _ = reply.send(Ok(()));
                    return Resting::Resume;
                }
                // Already stopped. Answering rather than refusing keeps a
                // second stop, or a stop racing a crash, from looking like an
                // error to whoever asked.
                Some(Control::Stop { reply }) => {
                    let _ = reply.send(Ok(()));
                }
                Some(Control::Exec { reply, .. }) => {
                    let _ = reply.send(Err("the server is not running".to_owned()));
                }
                Some(Control::SwitchConfig { instance, reply }) => {
                    let result = self.switch_to(*instance);
                    let _ = reply.send(result);
                }
                Some(Control::Shutdown { reply }) => {
                    let _ = reply.send(Ok(()));
                    return Resting::Done;
                }
                None => return Resting::Done,
            }
        }
    }

    async fn run_once(&mut self, control: &mut mpsc::Receiver<Control>) -> RunOutcome {
        let needs_local_http = self.instance.bridge.enabled;
        let command = launch::command_for(&self.instance.server, needs_local_http);
        let redacted = command.redacted(self.instance.server.gslt.as_ref());

        // Armed before the child starts, so the follow position is this run's
        // first byte. Arming after would race the engine's boot lines, and the
        // engine writes several before anything else happens.
        let mut tailer = Tailer::new(launch::log_file_for(&self.instance.server));
        tailer.poll();

        let spawned = process::spawn(
            &command,
            self.instance.server.working_dir.as_ref(),
            &child_environment(&self.instance.server),
            redacted.clone(),
        );

        let (mut child, mut output) = match spawned {
            Ok(pair) => pair,
            Err(error) => {
                tracing::error!("could not start the server: {error}");
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
                    self.instance.supervisor.restart,
                    self.instance.supervisor.backoff,
                ) {
                    Decision::RestartAfter(delay) => RunOutcome::RestartAfter(delay),
                    Decision::Stop => RunOutcome::Stopped,
                    Decision::GiveUp => RunOutcome::GaveUp,
                };
            }
        };

        let pid = child.pid().unwrap_or(0);
        tracing::info!("started the server as pid {pid}: {redacted}");
        self.publish(Event::ProcessStarted {
            pid,
            command: redacted,
        });

        let mut assembler = LineAssembler::new();
        let ready_pattern = self.instance.ready_pattern().to_owned();

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
            self.instance.supervisor.sample_interval_seconds.max(1),
        ));
        let mut exit_tick = tokio::time::interval(Duration::from_millis(250));

        let run_started = Instant::now();
        let start_deadline = match self.instance.supervisor.start_timeout_seconds {
            0 => None,
            seconds => Some(Duration::from_secs(seconds)),
        };
        let mut said_unhealthy = false;
        let mut exit_check_failed = false;
        let mut termination_failed = false;
        let mut requested_stop = false;
        let mut restart_requested = false;
        let mut shutting_down = false;
        // The command awaiting its reply, if any.
        let mut collecting: Option<PendingReply> = None;

        let exit_code = 'running: loop {
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

                    if !said_unhealthy
                        && let Some(deadline) = start_deadline
                        && run_started.elapsed() >= deadline
                        && self.tracker.state() == State::Starting
                    {
                        said_unhealthy = true;
                        self.tracker.set_state(State::Unhealthy);
                        let why = format!(
                            "the server has been starting for {}s without matching the ready \
                             pattern '{}'. Either the gamemode never logs that line, or the engine \
                             is stuck: it does not exit on a package resolution failure, it idles \
                             at the console. The process is still running and has not been touched.",
                            deadline.as_secs(),
                            ready_pattern,
                        );
                        tracing::warn!("{why}");
                        self.publish(Event::Unparsed { raw: why, origin: Origin::Cellar });
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

                _ = exit_tick.tick(), if !exit_check_failed => {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status.exit_code() as i32),
                        Ok(None) => {}
                        Err(error) => {
                            exit_check_failed = true;
                            termination_failed = true;
                            self.report_stop_failure(format!(
                                "could not inspect the managed child: {error}; it will not be replaced"
                            ));
                        }
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
                        Some(Control::SwitchConfig { instance, reply }) => {
                            let previous = self.instance.clone();
                            if let Err(why) = self.switch_to(*instance) {
                                let _ = reply.send(Err(why));
                            } else {
                                match self.graceful_stop(&mut child, &mut output, &mut assembler, &mut collecting, &ready_pattern, console_authoritative).await {
                                    Ok(status) => {
                                        restart_requested = true;
                                        let _ = reply.send(Ok(()));
                                        break status;
                                    }
                                    Err(error) => {
                                        termination_failed = true;
                                        self.instance = previous;
                                        let error = match self.prepare_hosting() {
                                            Ok(_) => error,
                                            Err(restore_error) => format!(
                                                "{error}; could not restore the previous hosting configuration: {restore_error}"
                                            ),
                                        };
                                        self.report_stop_failure(error.clone());
                                        let _ = reply.send(Err(error));
                                    }
                                }
                            }
                        }
                        Some(Control::Stop { reply }) => {
                            match self.graceful_stop(&mut child, &mut output, &mut assembler, &mut collecting, &ready_pattern, console_authoritative).await {
                                Ok(status) => {
                                    requested_stop = true;
                                    let _ = reply.send(Ok(()));
                                    break status;
                                }
                                Err(error) => {
                                    termination_failed = true;
                                    self.report_stop_failure(error.clone());
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                        Some(Control::Restart { reply }) => {
                            tracing::info!("restarting the server on request");
                            match self.graceful_stop(&mut child, &mut output, &mut assembler, &mut collecting, &ready_pattern, console_authoritative).await {
                                Ok(status) => {
                                    restart_requested = true;
                                    let _ = reply.send(Ok(()));
                                    break status;
                                }
                                Err(error) => {
                                    termination_failed = true;
                                    self.report_stop_failure(error.clone());
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                        // Cellar is exiting with the server still up. Stop it
                        // the same way an explicit stop would: the engine has
                        // no signal handler, so anything else skips the Steam
                        // logoff and the convar save.
                        Some(Control::Shutdown { reply }) => {
                            match self.graceful_stop(&mut child, &mut output, &mut assembler, &mut collecting, &ready_pattern, console_authoritative).await {
                                Ok(status) => {
                                    requested_stop = true;
                                    shutting_down = true;
                                    let _ = reply.send(Ok(()));
                                    break status;
                                }
                                Err(error) => {
                                    termination_failed = true;
                                    self.report_stop_failure(error.clone());
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                        None => {
                            match self.graceful_stop(&mut child, &mut output, &mut assembler, &mut collecting, &ready_pattern, console_authoritative).await {
                                Ok(status) => {
                                    requested_stop = true;
                                    break status;
                                }
                                Err(error) => {
                                    termination_failed = true;
                                    self.report_stop_failure(error);
                                    tracing::error!("the control channel closed before the server exit was confirmed; retaining the child handle until it exits");
                                    loop {
                                        match child.try_wait_code() {
                                            Ok(Some(status)) => {
                                                requested_stop = true;
                                                break 'running Some(status);
                                            }
                                            Ok(None) | Err(_) => tokio::time::sleep(EXIT_POLL_INTERVAL).await,
                                        }
                                    }
                                }
                            }
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

        let graceful = requested_stop || restart_requested;
        match (graceful, exit_code) {
            (true, code) => tracing::info!(
                "the server exited with {} after a stop Cellar asked for",
                describe_exit(code),
            ),
            (false, code) => tracing::warn!(
                "the server exited with {} without being asked to, after {}s",
                describe_exit(code),
                run_started.elapsed().as_secs(),
            ),
        }

        self.publish(Event::ProcessExited {
            code: exit_code,
            graceful,
        });

        self.outcome_after_exit(
            exit_code,
            requested_stop,
            restart_requested,
            shutting_down,
            termination_failed,
            run_started.elapsed(),
        )
    }

    fn outcome_after_exit(
        &mut self,
        exit_code: Option<i32>,
        requested_stop: bool,
        restart_requested: bool,
        shutting_down: bool,
        termination_failed: bool,
        run_elapsed: Duration,
    ) -> RunOutcome {
        if shutting_down {
            return RunOutcome::ShutDown;
        }

        if termination_failed {
            return RunOutcome::Stopped;
        }

        if restart_requested {
            self.restarts.record_healthy_run();
            return RunOutcome::RestartAfter(Duration::from_millis(500));
        }

        match self.restarts.on_exit(
            exit_code,
            requested_stop,
            run_elapsed,
            self.started.elapsed().as_secs(),
            self.instance.supervisor.restart,
            self.instance.supervisor.backoff,
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
    /// Returns an error unless the child exit is observed and reaped.
    async fn graceful_stop(
        &mut self,
        child: &mut impl TerminationChild,
        output: &mut mpsc::Receiver<Output>,
        assembler: &mut LineAssembler,
        pending: &mut Option<PendingReply>,
        ready_pattern: &str,
        console_is_the_source: bool,
    ) -> Result<Option<i32>, String> {
        self.tracker.set_state(State::Stopping);

        if child.send_command("quit").is_err() {
            tracing::warn!(
                "could not reach the console to send `quit`; killing the server, which skips the \
                 Steam logoff and the convar save"
            );
            return kill_and_confirm(child, KILL_CONFIRM_TIMEOUT)
                .await
                .map(Some);
        }
        tracing::info!(
            "sent `quit`; giving the engine {}s to run its nine shutdown steps",
            self.instance.supervisor.graceful_timeout_seconds.max(1),
        );

        let deadline =
            Duration::from_secs(self.instance.supervisor.graceful_timeout_seconds.max(1));
        let deadline_at = Instant::now() + deadline;
        loop {
            match child.try_wait_code() {
                Ok(Some(status)) => return Ok(Some(status)),
                Err(error) => {
                    return Err(format!("could not confirm that the server exited: {error}"));
                }
                Ok(None) => {}
            }

            if Instant::now() >= deadline_at {
                break;
            }

            // Keep draining while it shuts down; the shutdown log is exactly
            // what tells an operator the Steam logoff completed. Through
            // the same ingest as the rest of the run, so the dedup still
            // applies: the log file carries these lines too, and the final
            // tailer poll after this returns picks them up with their
            // logger names intact.
            match tokio::time::timeout(Duration::from_millis(100), output.recv()).await {
                Ok(Some(Output::Bytes(bytes))) => {
                    for line in assembler.push(&bytes) {
                        self.ingest(
                            &line,
                            Origin::Console,
                            ready_pattern,
                            pending,
                            console_is_the_source,
                        );
                    }
                }
                // A closed terminal is not proof that its child is gone. Keep
                // polling until it exits or the graceful deadline is reached.
                Ok(Some(Output::Eof)) | Ok(None) => {
                    let remaining = deadline_at.saturating_duration_since(Instant::now());
                    if let Ok(status) = wait_for_exit(child, remaining).await {
                        return Ok(Some(status));
                    }
                    break;
                }
                Err(_) => {}
            }
        }

        let why = format!(
            "the server did not exit within {}s of `quit`; killing it, which skips the \
             Steam logoff and the convar save",
            deadline.as_secs()
        );
        tracing::warn!("{why}");
        self.publish(Event::Unparsed {
            raw: why,
            origin: Origin::Cellar,
        });
        kill_and_confirm(child, KILL_CONFIRM_TIMEOUT)
            .await
            .map(Some)
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

        let event = grammar::classify(&parsed, origin, ready_pattern, &self.instance.profile);
        if let Event::ServerReady { hostname, .. } = &event {
            tracing::info!(
                "the server is ready and accepting players as '{}'",
                hostname
                    .as_deref()
                    .unwrap_or(&self.instance.server.hostname),
            );
        }
        self.publish(event);
    }
}

enum RunOutcome {
    Stopped,
    GaveUp,
    RestartAfter(Duration),
    /// Cellar itself is exiting.
    ShutDown,
}

/// What Cellar adds to the child's environment.
///
/// One entry, and deliberately not `FACEPUNCH_ENGINE`: that was measured on
/// 2026-09-01 to move neither `logs/` nor `data/`, from the process environment
/// or from the Wine registry, because the engine reads it with
/// `EnvironmentVariableTarget.User`. See `docs/ARCHITECTURE.md`.
fn child_environment(server: &ServerConfig) -> Vec<(String, String)> {
    let mut env = Vec::new();

    if server.launcher == Launcher::Wine
        && let Some(prefix) = &server.wine_prefix
    {
        env.push((
            "WINEPREFIX".to_owned(),
            prefix.to_string_lossy().into_owned(),
        ));
    }

    env
}

/// An exit status in words. `None` means the process died on a signal and
/// reported no code at all, which is not the same as exit 0.
fn describe_exit(code: Option<i32>) -> String {
    match code {
        Some(0) => "code 0, cleanly".to_owned(),
        Some(code) => format!("code {code}"),
        None => "no exit code, so it was killed by a signal".to_owned(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Resting {
    Resume,
    Done,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::path::PathBuf;

    use cellar_core::config::{Launcher, ServerConfig};

    use super::*;

    fn instance(data_dir: Option<PathBuf>) -> Instance {
        Instance {
            id: cellar_core::config::InstanceId::new("test").unwrap_or_else(|_| unreachable!()),
            scope: "test".to_owned(),
            enabled: true,
            required: true,
            player_ceiling: None,
            profile: Default::default(),
            server: ServerConfig {
                executable: PathBuf::from("/bin/true"),
                project: PathBuf::from("/tmp/a.sbproj"),
                launcher: Launcher::Native,
                hostname: "test".into(),
                ready_pattern: Some("Lobby created".into()),
                data_dir,
                ..ServerConfig::default()
            },
            supervisor: Default::default(),
            bridge: Default::default(),
        }
    }

    struct TestChild {
        command_result: io::Result<()>,
        exits: VecDeque<io::Result<Option<i32>>>,
        kill_result: io::Result<()>,
    }

    impl TerminationChild for TestChild {
        fn send_command(&self, _: &str) -> io::Result<()> {
            match &self.command_result {
                Ok(()) => Ok(()),
                Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
            }
        }

        fn try_wait_code(&mut self) -> io::Result<Option<i32>> {
            self.exits.pop_front().unwrap_or(Ok(None))
        }

        fn kill(&mut self) -> io::Result<()> {
            match &self.kill_result {
                Ok(()) => Ok(()),
                Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
            }
        }
    }

    #[tokio::test]
    async fn graceful_stop_waits_for_exit_after_the_terminal_closes() {
        let mut child = TestChild {
            command_result: Ok(()),
            exits: VecDeque::from([Ok(None), Ok(None), Ok(Some(0))]),
            kill_result: Ok(()),
        };
        let (mut supervisor, _, _) = Supervisor::new(instance(None));
        let (output_tx, mut output) = mpsc::channel(1);
        output_tx.send(Output::Eof).await.unwrap();
        drop(output_tx);
        let mut assembler = LineAssembler::new();
        let mut pending = None;

        let outcome = supervisor
            .graceful_stop(
                &mut child,
                &mut output,
                &mut assembler,
                &mut pending,
                "Lobby created",
                true,
            )
            .await;

        assert_eq!(outcome, Ok(Some(0)));
    }

    #[tokio::test]
    async fn a_failed_kill_is_not_reported_as_a_confirmed_exit() {
        let mut child = TestChild {
            command_result: Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed")),
            exits: VecDeque::new(),
            kill_result: Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
        };
        let (mut supervisor, _, _) = Supervisor::new(instance(None));
        let (_output_tx, mut output) = mpsc::channel(1);
        let mut assembler = LineAssembler::new();
        let mut pending = None;

        let error = supervisor
            .graceful_stop(
                &mut child,
                &mut output,
                &mut assembler,
                &mut pending,
                "Lobby created",
                true,
            )
            .await
            .expect_err("a kill failure leaves the child unconfirmed");

        assert!(error.contains("could not kill"), "{error}");
    }

    #[test]
    fn a_late_exit_after_failed_termination_never_restarts() {
        let mut server = instance(None);
        server.supervisor.restart = cellar_core::lifecycle::RestartPolicy::Always;
        let (mut supervisor, _, _) = Supervisor::new(server);

        let outcome = supervisor.outcome_after_exit(
            Some(1),
            false,
            false,
            false,
            true,
            Duration::from_secs(1),
        );

        assert!(matches!(outcome, RunOutcome::Stopped));
    }

    /// The ceiling is known before a player connects, or it is honestly zero.
    ///
    /// `+maxplayers` is not a convar and not a launch switch; the number lives
    /// in the project's `Metadata.MaxPlayers`, and until it was read the
    /// dashboard showed a ceiling of zero until the engine's status bar
    /// happened to mention one. The status bar still wins once it arrives:
    /// that is what the engine is actually enforcing.
    #[test]
    fn the_player_ceiling_is_seeded_from_the_project_and_then_the_engine_wins() {
        let mut declared = instance(None);
        declared.player_ceiling = Some(48);
        let (supervisor, _, _) = Supervisor::new(declared);
        assert_eq!(supervisor.tracker.snapshot().max_players, 48);

        // Nobody has read a project. Zero means unknown, and the dashboard
        // says so rather than showing "0/0" as if it were a limit.
        let (blind, _, _) = Supervisor::new(instance(None));
        assert_eq!(blind.tracker.snapshot().max_players, 0);
    }

    #[test]
    fn wine_gets_its_prefix_and_native_does_not() {
        let mut server = instance(None).server;
        server.launcher = Launcher::Wine;
        server.wine_prefix = Some(PathBuf::from("/srv/dev/wine"));
        assert_eq!(
            child_environment(&server),
            vec![("WINEPREFIX".to_owned(), "/srv/dev/wine".to_owned())]
        );

        // On Windows the setting means nothing, and passing it would put a
        // variable in the child's environment that only confuses whoever reads
        // it there later.
        server.launcher = Launcher::Native;
        assert!(child_environment(&server).is_empty());
    }

    /// Measured 2026-09-01: it moves neither `logs/` nor `data/`, from the
    /// process environment or from the Wine registry, because the engine reads
    /// it with `EnvironmentVariableTarget.User`. Setting it would be a variable
    /// that looks like it does something.
    #[test]
    fn facepunch_engine_is_not_passed_to_the_child() {
        let mut server = instance(None).server;
        server.launcher = Launcher::Wine;
        server.wine_prefix = Some(PathBuf::from("/srv/dev/wine"));

        assert!(
            !child_environment(&server)
                .iter()
                .any(|(key, _)| key == "FACEPUNCH_ENGINE")
        );
    }

    #[test]
    fn hosting_json_is_written_next_to_the_other_documents() {
        let dir = tempfile::tempdir().unwrap();
        let mut instance = instance(Some(dir.path().to_path_buf()));
        instance.bridge.enabled = true;
        instance.bridge.public_url = "http://127.0.0.1:8080".into();

        let (supervisor, _handle, _control) = Supervisor::new(instance);
        let message = supervisor.prepare_hosting().unwrap();
        assert!(message.contains("hosted"), "{message}");

        let written = std::fs::read_to_string(dir.path().join("hosting.json")).unwrap();
        assert!(written.contains("\"bridgeUrl\": \"http://127.0.0.1:8080\""));
    }

    /// A bridge with nowhere to write `hosting.json` would silently leave the
    /// gamemode on local files, which is the quiet failure worth refusing.
    #[test]
    fn a_bridge_without_a_data_dir_says_so_instead_of_going_quiet() {
        let mut instance = instance(None);
        instance.bridge.enabled = true;

        let (supervisor, _handle, _control) = Supervisor::new(instance);
        let error = supervisor.prepare_hosting().unwrap_err();
        assert!(error.contains("data_dir"), "{error}");
    }

    #[test]
    fn no_bridge_and_no_data_dir_is_simply_fine() {
        let (supervisor, _handle, _control) = Supervisor::new(instance(None));
        assert!(supervisor.prepare_hosting().is_ok());
    }
}

//! Supervising a local `mariadbd` for the lifetime of `cellar run`.
//!
//! A smaller sibling of `cellar_runtime::supervisor::Supervisor`: the same
//! restart/backoff shape, reusing `cellar_core::lifecycle::RestartTracker`
//! as-is, but none of the PTY/console-grammar machinery that exists there
//! specifically for the game server's console. `mariadbd` is a normal
//! console application with plain stdout/stderr, so this drives it with
//! `tokio::process` directly.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cellar_core::config::MariaDbConfig;
use cellar_core::lifecycle::{Decision, RestartTracker, State};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::provision;

/// What an outside caller can ask the supervisor to do. Deliberately no
/// `Exec`: nothing here runs operator commands the way the game console does.
#[derive(Debug)]
pub enum Control {
    /// Graceful stop: `mariadb-admin shutdown`, then wait, then kill.
    Stop { reply: oneshot::Sender<()> },
    /// Stop and start again, resetting the backoff.
    Restart { reply: oneshot::Sender<()> },
    /// Read the current state.
    Snapshot { reply: oneshot::Sender<Status> },
}

/// What the web UI's status card and `cellar mariadb status` read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub version: String,
    pub port: u16,
    pub data_dir: PathBuf,
}

/// Handle for talking to a running supervisor.
#[derive(Clone)]
pub struct Handle {
    control: mpsc::Sender<Control>,
}

impl Handle {
    pub async fn snapshot(&self) -> Option<Status> {
        let (reply, rx) = oneshot::channel();
        self.control.send(Control::Snapshot { reply }).await.ok()?;
        rx.await.ok()
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

    /// Block until the instance reports `Running`, or the timeout elapses.
    ///
    /// `cellar run` awaits this before opening the database pool: connecting
    /// while `mariadbd` is still initializing would just be the first of a
    /// string of retries, and this is the one place that already knows when
    /// to stop waiting.
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.snapshot().await
                && status.state == State::Running
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Owns the `mariadbd` child process.
pub struct Supervisor {
    config: MariaDbConfig,
    /// The app user's password, recovered from `CELLAR_DATABASE_URL` at
    /// startup (see `credentials::password_from_database_url`), needed to
    /// authenticate a graceful `mariadb-admin shutdown`.
    password: String,
    restarts: RestartTracker,
    started: Instant,
    state: State,
}

impl Supervisor {
    pub fn new(config: MariaDbConfig, password: String) -> (Self, Handle, mpsc::Receiver<Control>) {
        let (control_tx, control_rx) = mpsc::channel(16);
        let handle = Handle {
            control: control_tx,
        };

        (
            Self {
                config,
                password,
                restarts: RestartTracker::new(),
                started: Instant::now(),
                state: State::Stopped,
            },
            handle,
            control_rx,
        )
    }

    fn bin_dir(&self) -> PathBuf {
        // `Config::validate` refuses `mariadb.managed = true` without
        // `install_dir` set, and this is only ever constructed when managed,
        // so an absent value here would be that guarantee having failed.
        self.config
            .install_dir
            .clone()
            .unwrap_or_default()
            .join("bin")
    }

    fn status(&self) -> Status {
        Status {
            state: self.state,
            version: self.config.version.clone(),
            port: self.config.port,
            data_dir: self.config.data_dir.clone().unwrap_or_default(),
        }
    }

    /// Run until told to stop, or until the restart policy gives up.
    pub async fn run(mut self, mut control: mpsc::Receiver<Control>) {
        loop {
            match self.run_once(&mut control).await {
                RunOutcome::Stopped => {
                    self.state = State::Stopped;
                    return;
                }
                RunOutcome::GaveUp => {
                    self.state = State::CrashLooping;
                    tracing::error!(
                        "mariadb restarted too many times too fast; giving up. Check {} for why \
                         it keeps exiting.",
                        self.config
                            .data_dir
                            .as_ref()
                            .map(|d| d.display().to_string())
                            .unwrap_or_default()
                    );
                    return;
                }
                RunOutcome::RestartAfter(delay) => {
                    self.state = State::Backoff;

                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        message = control.recv() => {
                            match message {
                                Some(Control::Stop { reply }) => {
                                    let _ = reply.send(());
                                    self.state = State::Stopped;
                                    return;
                                }
                                Some(Control::Snapshot { reply }) => {
                                    let _ = reply.send(self.status());
                                }
                                Some(Control::Restart { reply }) => {
                                    self.restarts.record_healthy_run();
                                    let _ = reply.send(());
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
        let bin_dir = self.bin_dir();
        let data_dir = self.config.data_dir.clone().unwrap_or_default();

        let mut child = match provision::spawn(&bin_dir, &data_dir, self.config.port) {
            Ok(child) => child,
            Err(error) => {
                tracing::error!("could not start mariadbd: {error}");
                let now = self.started.elapsed().as_secs();
                return match self.restarts.on_exit(
                    None,
                    false,
                    Duration::ZERO,
                    now,
                    self.config.restart,
                    self.config.backoff,
                ) {
                    Decision::RestartAfter(delay) => RunOutcome::RestartAfter(delay),
                    Decision::Stop => RunOutcome::Stopped,
                    Decision::GiveUp => RunOutcome::GaveUp,
                };
            }
        };

        self.state = State::Starting;
        tracing::info!("mariadbd starting on 127.0.0.1:{}", self.config.port);

        let mut exit_tick = tokio::time::interval(Duration::from_millis(250));
        let mut ready_tick = tokio::time::interval(Duration::from_millis(250));
        let run_started = Instant::now();
        let mut requested_stop = false;
        let mut restart_requested = false;

        let exit_code = loop {
            tokio::select! {
                _ = exit_tick.tick() => {
                    match child.try_wait() {
                        Ok(Some(status)) => break status.code(),
                        Ok(None) => {}
                        Err(_) => break None,
                    }
                }

                _ = ready_tick.tick(), if self.state == State::Starting => {
                    if tokio::net::TcpStream::connect(("127.0.0.1", self.config.port)).await.is_ok() {
                        self.state = State::Running;
                        tracing::info!("mariadbd accepting connections on 127.0.0.1:{}", self.config.port);
                    }
                }

                message = control.recv() => {
                    match message {
                        Some(Control::Snapshot { reply }) => {
                            let _ = reply.send(self.status());
                        }
                        Some(Control::Stop { reply }) => {
                            requested_stop = true;
                            let status = self.graceful_stop(&mut child, &bin_dir).await;
                            let _ = reply.send(());
                            break status;
                        }
                        Some(Control::Restart { reply }) => {
                            restart_requested = true;
                            let status = self.graceful_stop(&mut child, &bin_dir).await;
                            let _ = reply.send(());
                            break status;
                        }
                        None => {
                            requested_stop = true;
                            break self.graceful_stop(&mut child, &bin_dir).await;
                        }
                    }
                }
            }
        };

        if restart_requested {
            self.restarts.record_healthy_run();
            return RunOutcome::RestartAfter(Duration::from_millis(500));
        }

        match self.restarts.on_exit(
            exit_code,
            requested_stop,
            run_started.elapsed(),
            self.started.elapsed().as_secs(),
            self.config.restart,
            self.config.backoff,
        ) {
            Decision::RestartAfter(delay) => RunOutcome::RestartAfter(delay),
            Decision::Stop => RunOutcome::Stopped,
            Decision::GiveUp => RunOutcome::GaveUp,
        }
    }

    /// `mariadb-admin shutdown` as the app user, then wait, then kill.
    ///
    /// A killed `mariadbd` is not catastrophic, InnoDB crash-recovers its redo
    /// log on the next start, but it is slower and logs a warning, the same
    /// tone `cellar_runtime::supervisor::graceful_stop` takes with the game
    /// server's own kill fallback.
    async fn graceful_stop(
        &mut self,
        child: &mut tokio::process::Child,
        bin_dir: &std::path::Path,
    ) -> Option<i32> {
        self.state = State::Stopping;

        let deadline = Duration::from_secs(self.config.graceful_timeout_seconds.max(1));
        let asked = provision::run_admin_shutdown(
            bin_dir,
            self.config.port,
            &self.config.username,
            Some(&self.password),
        )
        .await
        .is_ok();

        if asked && let Ok(Ok(status)) = tokio::time::timeout(deadline, child.wait()).await {
            return status.code();
        }

        tracing::warn!(
            "mariadbd did not exit within {}s of a clean shutdown request; killing it",
            deadline.as_secs()
        );
        let _ = child.kill().await;
        child.wait().await.ok().and_then(|status| status.code())
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
    use super::*;

    fn config(install_dir: PathBuf, data_dir: PathBuf, port: u16) -> MariaDbConfig {
        MariaDbConfig {
            managed: true,
            version: "11.4.5".to_owned(),
            sha256: Some("a".repeat(64)),
            install_dir: Some(install_dir),
            data_dir: Some(data_dir),
            port,
            database: "cellar".to_owned(),
            username: "cellar".to_owned(),
            ..Default::default()
        }
    }

    /// A stand-in binary directory: `spawn` just needs *something* runnable
    /// at `bin/mariadbd.exe`. On the platform tests actually run on this is
    /// never really invoked as `mariadbd`; the point of these tests is the
    /// restart/backoff wiring, not a real server, mirroring how
    /// `cellar-runtime`'s own supervisor tests never spawn a real
    /// `sbox-server.exe` either.
    #[test]
    fn a_fresh_supervisor_reports_stopped() {
        let (supervisor, _handle, _control) = Supervisor::new(
            config(PathBuf::from("/nowhere"), PathBuf::from("/nowhere/data"), 0),
            "x".into(),
        );
        assert_eq!(supervisor.status().state, State::Stopped);
    }

    #[tokio::test]
    async fn a_snapshot_answers_before_anything_is_spawned() {
        let (supervisor, handle, control) = Supervisor::new(
            config(PathBuf::from("/nowhere"), PathBuf::from("/nowhere/data"), 0),
            "x".into(),
        );
        let running = tokio::spawn(supervisor.run(control));

        let status = handle.snapshot().await.unwrap();
        assert_eq!(status.version, "11.4.5");

        handle.stop().await;
        let _ = running.await;
    }

    #[tokio::test]
    async fn waiting_ready_times_out_when_nothing_ever_starts() {
        let (supervisor, handle, control) = Supervisor::new(
            config(PathBuf::from("/nowhere"), PathBuf::from("/nowhere/data"), 0),
            "x".into(),
        );
        let running = tokio::spawn(supervisor.run(control));

        let ready = handle.wait_ready(Duration::from_millis(300)).await;
        assert!(!ready, "nothing was ever spawned at this path");

        handle.stop().await;
        let _ = running.await;
    }
}

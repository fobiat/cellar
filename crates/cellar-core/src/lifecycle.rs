//! The server's lifecycle, and when to restart it.
//!
//! Kept pure so the interesting cases (a crash loop, a server that exits zero
//! on its own, a stop the operator asked for) are unit tests rather than things
//! discovered in production at 3am.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where the supervised server is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not running, and not trying to.
    Stopped,
    /// Process spawned, no readiness signal seen yet.
    Starting,
    /// Readiness seen. Accepting players.
    Running,
    /// Process still up, readiness never arrived within the start timeout.
    ///
    /// Distinct from `Starting` because the two look identical and only one of
    /// them is going to resolve. Distinct from `CrashLooping` because nothing
    /// has crashed: the process is alive, the ports may well be bound, and the
    /// cause may be nothing worse than a `ready_pattern` this gamemode never
    /// emits. Not ready, so a readiness probe still refuses it.
    Unhealthy,
    /// A graceful stop is in flight: `quit` sent, waiting for exit.
    Stopping,
    /// Exited unexpectedly. Waiting out the restart backoff.
    Backoff,
    /// Restarted too often, too fast. Cellar has stopped trying.
    CrashLooping,
}

impl State {
    /// Whether the server should be answering a readiness probe.
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Running)
    }

    /// Whether a process exists right now.
    pub fn has_process(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Stopping | Self::Unhealthy
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Unhealthy => "unhealthy",
            Self::Stopping => "stopping",
            Self::Backoff => "backoff",
            Self::CrashLooping => "crash_looping",
        }
    }
}

/// How Cellar reacts to the server exiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RestartPolicy {
    /// Never restart. The container runtime or the operator decides.
    Never,
    /// Restart on any exit, clean or not.
    Always,
    /// Restart only when the exit was not asked for and not clean.
    #[default]
    OnFailure,
}

/// Exponential backoff with a ceiling, plus crash-loop detection.
///
/// The window matters more than the count. A server that has restarted twenty
/// times over a month is fine; one that has restarted five times in two minutes
/// is not going to fix itself, and continuing to restart it hides the fault and
/// burns the Steam master-list registration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BackoffPolicy {
    #[serde(with = "humantime_seconds")]
    pub initial: Duration,
    #[serde(with = "humantime_seconds")]
    pub maximum: Duration,
    pub multiplier: f32,
    /// Restarts within `window` before Cellar gives up.
    pub crash_loop_threshold: u32,
    #[serde(with = "humantime_seconds")]
    pub window: Duration,
    /// Uptime after which a run counts as healthy and the streak resets.
    #[serde(with = "humantime_seconds")]
    pub healthy_after: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(2),
            maximum: Duration::from_secs(120),
            multiplier: 2.0,
            crash_loop_threshold: 5,
            window: Duration::from_secs(300),
            // A dedicated server that survived a minute got past map load and
            // package compilation, which is where the repeatable failures are.
            healthy_after: Duration::from_secs(60),
        }
    }
}

/// Serde helper: durations in a config file read better as whole seconds than
/// as a `{ secs, nanos }` table.
mod humantime_seconds {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

/// What the supervisor should do after an exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Restart after waiting.
    RestartAfter(Duration),
    /// Do not restart; this exit was expected or policy forbids it.
    Stop,
    /// Restarting is not helping. Stop and say so loudly.
    GiveUp,
}

/// Tracks consecutive failures so the backoff and the crash-loop check agree.
#[derive(Debug, Clone, Copy, Default)]
pub struct RestartTracker {
    consecutive_failures: u32,
    /// Seconds since Cellar started, of the first failure in the current window.
    window_opened_at: Option<u64>,
    failures_in_window: u32,
}

impl RestartTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// A run that lasted long enough to count as healthy clears the streak.
    pub fn record_healthy_run(&mut self) {
        self.consecutive_failures = 0;
        self.window_opened_at = None;
        self.failures_in_window = 0;
    }

    /// Decide what to do about an exit.
    ///
    /// `now_seconds` is monotonic seconds since Cellar started, not wall clock:
    /// a clock step must not open or close a crash-loop window.
    pub fn on_exit(
        &mut self,
        exit_code: Option<i32>,
        requested: bool,
        uptime: Duration,
        now_seconds: u64,
        policy: RestartPolicy,
        backoff: BackoffPolicy,
    ) -> Decision {
        if requested || policy == RestartPolicy::Never {
            return Decision::Stop;
        }

        let clean = exit_code == Some(0);
        if clean && policy == RestartPolicy::OnFailure {
            return Decision::Stop;
        }

        if uptime >= backoff.healthy_after {
            self.record_healthy_run();
        }

        // Reopen the window if the last failure was long enough ago.
        match self.window_opened_at {
            Some(opened) if now_seconds.saturating_sub(opened) > backoff.window.as_secs() => {
                self.window_opened_at = Some(now_seconds);
                self.failures_in_window = 0;
            }
            None => self.window_opened_at = Some(now_seconds),
            Some(_) => {}
        }

        self.failures_in_window += 1;
        self.consecutive_failures += 1;

        if self.failures_in_window >= backoff.crash_loop_threshold {
            return Decision::GiveUp;
        }

        Decision::RestartAfter(delay_for(self.consecutive_failures, backoff))
    }
}

/// Backoff delay for the nth consecutive failure, n starting at 1.
pub fn delay_for(consecutive_failures: u32, backoff: BackoffPolicy) -> Duration {
    if consecutive_failures <= 1 {
        return backoff.initial;
    }

    let exponent = consecutive_failures.saturating_sub(1).min(16);
    let scaled = backoff.initial.as_secs_f32() * backoff.multiplier.powi(exponent as i32);

    if !scaled.is_finite() || scaled >= backoff.maximum.as_secs_f32() {
        return backoff.maximum;
    }

    Duration::from_secs_f32(scaled)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn policy() -> BackoffPolicy {
        BackoffPolicy::default()
    }

    #[test]
    fn a_requested_stop_never_restarts() {
        let mut tracker = RestartTracker::new();
        let decision = tracker.on_exit(
            Some(0),
            true,
            Duration::from_secs(600),
            600,
            RestartPolicy::Always,
            policy(),
        );
        assert_eq!(decision, Decision::Stop);
    }

    #[test]
    fn a_clean_exit_stops_under_on_failure_but_restarts_under_always() {
        let mut tracker = RestartTracker::new();
        assert_eq!(
            tracker.on_exit(
                Some(0),
                false,
                Duration::from_secs(5),
                5,
                RestartPolicy::OnFailure,
                policy()
            ),
            Decision::Stop
        );

        let mut tracker = RestartTracker::new();
        assert!(matches!(
            tracker.on_exit(
                Some(0),
                false,
                Duration::from_secs(5),
                5,
                RestartPolicy::Always,
                policy()
            ),
            Decision::RestartAfter(_)
        ));
    }

    #[test]
    fn backoff_grows_and_then_stops_growing() {
        let p = policy();
        assert_eq!(delay_for(1, p), Duration::from_secs(2));
        assert_eq!(delay_for(2, p), Duration::from_secs(4));
        assert_eq!(delay_for(3, p), Duration::from_secs(8));
        assert_eq!(delay_for(50, p), p.maximum, "capped, never unbounded");
    }

    #[test]
    fn five_fast_failures_is_a_crash_loop() {
        let mut tracker = RestartTracker::new();
        let p = policy();

        for second in 0..4 {
            let decision = tracker.on_exit(
                Some(1),
                false,
                Duration::from_secs(1),
                second,
                RestartPolicy::OnFailure,
                p,
            );
            assert!(
                matches!(decision, Decision::RestartAfter(_)),
                "failure {second}"
            );
        }

        assert_eq!(
            tracker.on_exit(
                Some(1),
                false,
                Duration::from_secs(1),
                5,
                RestartPolicy::OnFailure,
                p
            ),
            Decision::GiveUp
        );
    }

    #[test]
    fn failures_spread_beyond_the_window_are_not_a_crash_loop() {
        let mut tracker = RestartTracker::new();
        let p = policy();
        let mut now = 0u64;

        for _ in 0..10 {
            let decision = tracker.on_exit(
                Some(1),
                false,
                Duration::from_secs(1),
                now,
                RestartPolicy::OnFailure,
                p,
            );
            assert!(matches!(decision, Decision::RestartAfter(_)));
            now += p.window.as_secs() + 1;
        }
    }

    #[test]
    fn a_long_healthy_run_clears_the_streak() {
        let mut tracker = RestartTracker::new();
        let p = policy();

        tracker.on_exit(
            Some(1),
            false,
            Duration::from_secs(1),
            0,
            RestartPolicy::OnFailure,
            p,
        );
        tracker.on_exit(
            Some(1),
            false,
            Duration::from_secs(1),
            1,
            RestartPolicy::OnFailure,
            p,
        );
        assert_eq!(tracker.consecutive_failures(), 2);

        // An exit after a healthy run resets first, so this is failure number one again.
        let decision = tracker.on_exit(
            Some(1),
            false,
            p.healthy_after + Duration::from_secs(1),
            500,
            RestartPolicy::OnFailure,
            p,
        );
        assert_eq!(decision, Decision::RestartAfter(p.initial));
        assert_eq!(tracker.consecutive_failures(), 1);
    }

    #[test]
    fn a_signal_kill_has_no_exit_code_and_still_restarts() {
        let mut tracker = RestartTracker::new();
        assert!(matches!(
            tracker.on_exit(
                None,
                false,
                Duration::from_secs(3),
                3,
                RestartPolicy::OnFailure,
                policy()
            ),
            Decision::RestartAfter(_)
        ));
    }

    #[test]
    fn state_readiness_is_only_running() {
        assert!(State::Running.is_ready());
        for state in [
            State::Stopped,
            State::Starting,
            State::Unhealthy,
            State::Stopping,
            State::Backoff,
            State::CrashLooping,
        ] {
            assert!(!state.is_ready(), "{state:?} must not report ready");
        }
    }

    /// The point of the state: it says the process is there and not serving.
    /// Reading it as "gone" would be as wrong as reading it as "starting".
    #[test]
    fn an_unhealthy_server_is_still_a_running_process() {
        assert!(State::Unhealthy.has_process());
        assert!(!State::Unhealthy.is_ready());
    }
}

//! Every recurring job in one register, so an operator can see them.
//!
//! Cellar had five loops that each slept and did a thing, spawned from
//! `runner.rs` and invisible from anywhere else. Nothing said when a backup
//! last ran, whether it worked, or when the next one is due, and nothing could
//! ask for one now. Worst of it: `database.event_retention_days` was
//! configured and no loop existed at all, so the setting did nothing.
//!
//! Two of the five loops are deliberately **not** here. The supervisor's tail
//! tick and the MariaDB supervisor's are a state machine's clock inside a
//! `select!`, not jobs: they have no result to report and running one "now"
//! means nothing.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// What a job returns: a sentence for the operator, or the reason it failed.
pub type Outcome = Result<String, String>;

type Work = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Outcome> + Send>> + Send + Sync>;

/// What a job is, before it has ever run.
#[derive(Debug, Clone)]
pub struct Spec {
    /// Stable, lowercase, hyphenated. It is the id in a URL and in the UI.
    pub name: String,
    /// One line saying what the job does, shown beside it.
    pub description: String,
    pub interval: Duration,
    /// Whether to run once at startup rather than waiting out the first
    /// interval. True for the cheap read-only checks, false for anything that
    /// writes: a Cellar restarted in a loop must not take a backup each time.
    pub at_startup: bool,
}

/// What a job has done, for `/api/jobs` and the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub name: String,
    pub description: String,
    pub interval_seconds: u64,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    /// `None` until it has run once. `Some(true)` for a run that worked.
    pub last_ok: Option<bool>,
    pub last_detail: String,
    pub running: bool,
    pub runs: u64,
    pub failures: u64,
}

#[derive(Debug, Default, Clone)]
struct History {
    last_run: Option<DateTime<Utc>>,
    last_ok: Option<bool>,
    last_detail: String,
    running: bool,
    runs: u64,
    failures: u64,
}

struct Job {
    spec: Spec,
    work: Work,
    history: Mutex<History>,
    /// Notified by "run now". The loop selects on it and its sleep, so an
    /// asked-for run also resets the timer: two backups a second apart because
    /// somebody pressed the button just before the interval elapsed is not what
    /// anybody means by "run now".
    wake: tokio::sync::Notify,
}

impl Job {
    /// When the next automatic run is due.
    ///
    /// Measured from the last run rather than from a fixed schedule, which is
    /// what makes "run now" push the next one out by a full interval.
    fn next_run(&self, history: &History) -> Option<DateTime<Utc>> {
        let last = history.last_run?;
        chrono::Duration::from_std(self.spec.interval)
            .ok()
            .map(|interval| last + interval)
    }

    fn status(&self) -> Status {
        // A poisoned lock here would mean a panic inside `status` or `record`,
        // neither of which does anything that can panic. Recovering the guard
        // is still better than taking the whole web surface down with it.
        let history = self.history.lock().unwrap_or_else(|held| held.into_inner());
        Status {
            name: self.spec.name.clone(),
            description: self.spec.description.clone(),
            interval_seconds: self.spec.interval.as_secs(),
            last_run: history.last_run,
            next_run: self.next_run(&history),
            last_ok: history.last_ok,
            last_detail: history.last_detail.clone(),
            running: history.running,
            runs: history.runs,
            failures: history.failures,
        }
    }

    async fn run_once(&self) {
        {
            let mut history = self.history.lock().unwrap_or_else(|held| held.into_inner());
            history.running = true;
        }

        let outcome = (self.work)().await;

        let mut history = self.history.lock().unwrap_or_else(|held| held.into_inner());
        history.running = false;
        history.last_run = Some(Utc::now());
        history.runs += 1;
        match outcome {
            Ok(detail) => {
                history.last_ok = Some(true);
                history.last_detail = detail;
            }
            Err(why) => {
                history.last_ok = Some(false);
                history.failures += 1;
                history.last_detail = why.clone();
                tracing::error!("scheduled job '{}' failed: {why}", self.spec.name);
            }
        }
    }
}

/// The register. Built at startup, fixed after that.
#[derive(Default)]
pub struct Scheduler {
    jobs: Vec<Arc<Job>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a job. The closure is called on every run, so it owns whatever it
    /// needs rather than borrowing from the caller's stack.
    pub fn register<F, Fut>(&mut self, spec: Spec, work: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Outcome> + Send + 'static,
    {
        self.jobs.push(Arc::new(Job {
            spec,
            work: Arc::new(move || Box::pin(work())),
            history: Mutex::new(History::default()),
            wake: tokio::sync::Notify::new(),
        }));
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// One task per job. Returns immediately.
    pub fn start(self: &Arc<Self>) {
        for job in &self.jobs {
            let job = job.clone();
            tokio::spawn(async move {
                if job.spec.at_startup {
                    job.run_once().await;
                }
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(job.spec.interval) => {}
                        _ = job.wake.notified() => {}
                    }
                    job.run_once().await;
                }
            });
        }
    }

    pub fn statuses(&self) -> Vec<Status> {
        self.jobs.iter().map(|job| job.status()).collect()
    }

    /// Ask for a run now. Returns false when no job goes by that name.
    ///
    /// It nudges the job's own loop rather than running the work here, so a job
    /// cannot be running twice at once however many operators press the button.
    pub fn run_now(&self, name: &str) -> bool {
        let Some(job) = self.jobs.iter().find(|job| job.spec.name == name) else {
            return false;
        };
        job.wake.notify_one();
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn spec(name: &str, seconds: u64) -> Spec {
        Spec {
            name: name.to_owned(),
            description: "a job".to_owned(),
            interval: Duration::from_secs(seconds),
            at_startup: false,
        }
    }

    #[tokio::test]
    async fn a_job_that_has_never_run_has_no_next_run() {
        let mut scheduler = Scheduler::new();
        scheduler.register(spec("nothing", 3600), || async { Ok(String::new()) });

        let status = &scheduler.statuses()[0];
        assert_eq!(status.last_run, None);
        // Not "now plus an interval". The job has not run, so there is nothing
        // to measure from, and inventing a time would be a schedule the
        // scheduler does not actually keep.
        assert_eq!(status.next_run, None);
        assert_eq!(status.last_ok, None);
        assert_eq!(status.runs, 0);
    }

    #[tokio::test]
    async fn a_failure_is_counted_and_its_reason_kept() {
        let mut scheduler = Scheduler::new();
        scheduler.register(spec("fails", 3600), || async {
            Err("the dump directory is read-only".to_owned())
        });
        scheduler.jobs[0].run_once().await;

        let status = &scheduler.statuses()[0];
        assert_eq!(status.last_ok, Some(false));
        assert_eq!(status.failures, 1);
        assert_eq!(status.runs, 1);
        assert_eq!(status.last_detail, "the dump directory is read-only");
        // A failed run is still a run, so the next one is an interval away
        // rather than immediate. A job that fails instantly and retries
        // instantly is a busy loop against whatever is already broken.
        assert!(status.next_run.is_some());
    }

    #[tokio::test]
    async fn run_now_wakes_the_job_and_pushes_the_next_run_out() {
        static RUNS: AtomicU32 = AtomicU32::new(0);
        let mut scheduler = Scheduler::new();
        // An hour, so nothing but the nudge can make this run.
        scheduler.register(spec("backup", 3600), || async {
            RUNS.fetch_add(1, Ordering::SeqCst);
            Ok("wrote a dump".to_owned())
        });
        let scheduler = Arc::new(scheduler);
        scheduler.start();

        assert!(scheduler.run_now("backup"));
        for _ in 0..200 {
            if RUNS.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
        let status = &scheduler.statuses()[0];
        assert_eq!(status.last_ok, Some(true));
        let next = status.next_run.expect("it has run, so it has a next run");
        assert!(
            (next - Utc::now()).num_minutes() >= 58,
            "next run is {next}"
        );
    }

    #[tokio::test]
    async fn asking_for_a_job_that_does_not_exist_says_so() {
        let scheduler = Arc::new(Scheduler::new());
        assert!(!scheduler.run_now("no-such-job"));
    }
}

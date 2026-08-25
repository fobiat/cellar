//! End-to-end: the real supervisor, a real pseudo-terminal, a fake engine.
//!
//! These live here rather than in `cellar-runtime` because `CARGO_BIN_EXE_*` is
//! only set for binaries in the same package, and pointing the supervisor at a
//! path guessed from `target/` is the kind of test that passes locally and fails
//! in CI.
//!
//! What they cover is the part unit tests cannot: that a command typed into a
//! pty is read by a child that believes it has a terminal, that the log file and
//! the console are both parsed into one event stream, and that a graceful stop
//! reaches the engine's shutdown path instead of killing it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use cellar_core::config::{Config, Launcher, ServerConfig};
use cellar_core::event::Event;
use cellar_runtime::Supervisor;
use tokio::sync::broadcast;

const FAKE_SERVER: &str = env!("CARGO_BIN_EXE_cellar-fake-server");

fn config(log_file: PathBuf, extra: &[&str]) -> Config {
    Config {
        server: ServerConfig {
            executable: PathBuf::from(FAKE_SERVER),
            project: PathBuf::from("/tmp/applejackrp.sbproj"),
            launcher: Launcher::Native,
            working_dir: None,
            log_file: Some(log_file.clone()),
            hostname: "AppleJackRP Dev".to_owned(),
            gslt: None,
            direct_connect: false,
            port: 27015,
            query_port: 27016,
            ready_pattern: cellar_core::grammar::DEFAULT_READY_PATTERN.to_owned(),
            extra_args: {
                let mut args = vec!["--log-file".to_owned(), log_file.display().to_string()];
                args.extend(extra.iter().map(|s| (*s).to_owned()));
                args
            },
            data_dir: None,
        },
        supervisor: Default::default(),
        bridge: Default::default(),
        database: Default::default(),
        web: Default::default(),
        notify: Default::default(),
        update: Default::default(),
    }
}

/// Wait for an event matching `predicate`, or fail after `timeout`.
async fn wait_for<F>(
    events: &mut broadcast::Receiver<Event>,
    timeout: Duration,
    predicate: F,
) -> Event
where
    F: Fn(&Event) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for an event");

        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(event)) if predicate(&event) => return event,
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("the event stream closed"),
            Err(_) => panic!("timed out waiting for an event"),
        }
    }
}

#[tokio::test]
async fn the_server_starts_becomes_ready_and_stops_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path().join("logs/sbox-server.log"), &[]);

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ProcessStarted { .. })
    })
    .await;

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ServerReady { .. })
    })
    .await;

    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.state.is_ready(), "readiness reaches the snapshot");

    handle.stop().await;

    let exit = wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ProcessExited { .. })
    })
    .await;

    // `quit`, not a kill. A kill skips the Steam logoff and the convar save.
    match exit {
        Event::ProcessExited { code, graceful } => {
            assert_eq!(code, Some(0), "the engine exited through its own shutdown");
            assert!(graceful);
        }
        other => panic!("expected an exit, got {other:?}"),
    }

    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

#[tokio::test]
async fn players_joining_and_leaving_reach_the_roster() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path().join("logs/sbox-server.log"), &["--players", "2"]);

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ServerReady { .. })
    })
    .await;

    // Two joined at boot; wait for the second to be seen.
    wait_for(
        &mut events,
        Duration::from_secs(10),
        |e| matches!(e, Event::PlayerJoined { steam_id, .. } if *steam_id == 76561198000000001),
    )
    .await;

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.players.len(), 2, "{:?}", snapshot.players);

    // Drive one out through the console.
    handle.exec("leave", "test").await.unwrap();

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::PlayerLeft { .. })
    })
    .await;

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.players.len(), 1);

    handle.stop().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

/// The headline claim: the admin surface is drivable with no gamemode change.
#[tokio::test]
async fn a_console_command_returns_its_reply() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path().join("logs/sbox-server.log"), &[]);

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ServerReady { .. })
    })
    .await;

    let reply = handle.exec("applejack_features", "test").await.unwrap();
    let text = reply.join("\n");

    assert!(text.contains("ui.menu.admin"), "reply was: {text}");
    assert!(text.contains("41 feature(s)"), "reply was: {text}");

    // Two regressions, both found by running this against a live server rather
    // than by reading the code.
    //
    // The console and the log file carry the same lines, so parsing both counted
    // every line twice and the operator saw the feature list rendered twice.
    let repeats = reply
        .iter()
        .filter(|line| line.contains("41 feature(s)"))
        .count();
    assert_eq!(
        repeats, 1,
        "each line once, not once per channel: {reply:?}"
    );

    // A pty echoes what is typed into it, so the command came back as the first
    // line of its own reply.
    assert!(
        !reply.iter().any(|line| line.trim() == "applejack_features"),
        "the echoed command is not part of the reply: {reply:?}"
    );

    handle.stop().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

/// A server that ignores `quit` must still be stopped, and the escalation must
/// be reported rather than silent.
#[tokio::test]
async fn a_server_that_refuses_to_quit_is_killed_after_the_grace_period() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path().join("logs/sbox-server.log"), &["--ignore-quit"]);
    config.supervisor.graceful_timeout_seconds = 2;

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ServerReady { .. })
    })
    .await;

    let started = std::time::Instant::now();
    handle.stop().await;

    wait_for(&mut events, Duration::from_secs(15), |e| {
        matches!(e, Event::ProcessExited { .. })
    })
    .await;

    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "it waited out the grace period before killing"
    );

    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

/// A server that never becomes ready must never report ready, however long it
/// runs. This is what stops a rollout sending players at a server loading a map.
#[tokio::test]
async fn a_hanging_server_never_reports_ready() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path().join("logs/sbox-server.log"), &["--hang"]);
    config.supervisor.graceful_timeout_seconds = 1;

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ProcessStarted { .. })
    })
    .await;

    tokio::time::sleep(Duration::from_secs(2)).await;

    let snapshot = handle.snapshot().await.unwrap();
    assert!(!snapshot.state.is_ready(), "state was {:?}", snapshot.state);

    handle.stop().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
}

#[tokio::test]
async fn resource_samples_arrive_for_the_running_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path().join("logs/sbox-server.log"), &[]);
    config.supervisor.sample_interval_seconds = 1;

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    let sample = wait_for(
        &mut events,
        Duration::from_secs(15),
        |e| matches!(e, Event::Resources(s) if s.memory_bytes > 0),
    )
    .await;

    match sample {
        Event::Resources(sample) => assert!(sample.process_count >= 1),
        other => panic!("expected a sample, got {other:?}"),
    }

    handle.stop().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

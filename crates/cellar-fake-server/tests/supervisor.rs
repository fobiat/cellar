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
        instances: Default::default(),
        server: Some(ServerConfig {
            executable: PathBuf::from(FAKE_SERVER),
            project: PathBuf::from("/tmp/applejackrp.sbproj"),
            game: None,
            map: None,
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
        }),
        supervisor: Default::default(),
        bridge: Default::default(),
        database: Default::default(),
        web: Default::default(),
        notify: Default::default(),
        update: Default::default(),
        mariadb: Default::default(),
        backup: Default::default(),
        release: Default::default(),
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

    handle.shutdown().await;

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

    handle.shutdown().await;
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

    let dispatched = std::time::Instant::now();
    let reply = handle.exec("applejack_features", "test").await.unwrap();
    let elapsed = dispatched.elapsed();
    let text = reply.join("\n");

    // The reply is bracketed, not timed. `ConsoleInput.OnEnter` echoes
    // `> {command}` before dispatching and `RedrawInputLine` repaints the status
    // block once the ConCmd returns, so the answer arrives as soon as the server
    // is done. If this ever creeps up to REPLY_TIMEOUT the brackets have stopped
    // being recognised and the backstop is silently doing the work.
    assert!(
        elapsed < Duration::from_millis(1500),
        "the reply should be bracketed, not waited out; took {elapsed:?}"
    );

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

    handle.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

/// The configuration round trip, end to end: read what the server is set to,
/// write a change, and read it back.
///
/// The parsers are unit-tested against the formats read out of
/// `FeatureDirector.cs` and `SettingDirector.cs`. This proves they also match
/// what actually arrives through a pty, a log file and the reply window.
#[tokio::test]
async fn the_configuration_can_be_captured_changed_and_read_back() {
    use cellar_core::convar;

    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path().join("logs/sbox-server.log"), &[]);

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ServerReady { .. })
    })
    .await;

    let features =
        convar::parse_features(&handle.exec("applejack_features", "test").await.unwrap());
    assert!(!features.is_empty(), "the listing parsed into nothing");

    let admin = features
        .iter()
        .find(|feature| feature.id == "ui.menu.admin")
        .expect("ui.menu.admin is in the catalogue");
    assert!(!admin.enabled, "it ships off");
    assert!(admin.toggle.is_writable());

    let core = features
        .iter()
        .find(|feature| feature.id == "economy.manufacture")
        .expect("a core feature is in the catalogue");
    assert!(
        !core.toggle.is_writable(),
        "core features cannot be toggled"
    );

    let settings =
        convar::parse_settings(&handle.exec("applejack_settings", "test").await.unwrap());
    let arrest = settings
        .iter()
        .find(|setting| setting.id == "crime.arrest_seconds")
        .expect("crime.arrest_seconds is catalogued");
    assert_eq!(arrest.value, "300");
    assert!(arrest.is_default());

    // Plan the change the way `cellar settings apply` does, then send it.
    let current = convar::Snapshot {
        features: features.clone(),
        settings: settings.clone(),
        ..Default::default()
    };

    let desired = convar::Snapshot {
        features: vec![convar::Feature {
            id: "ui.menu.admin".into(),
            enabled: true,
            is_default: false,
            toggle: convar::Toggle::Live,
            title: String::new(),
        }],
        settings: vec![convar::Setting {
            id: "crime.arrest_seconds".into(),
            value: "600".into(),
            default: "300".into(),
            bounds: None,
            source: None,
        }],
        ..Default::default()
    };

    let changes = convar::plan(&current, &desired);
    assert_eq!(changes.len(), 2, "{changes:#?}");

    for change in &changes {
        handle.exec(&change.command, "test").await.unwrap();
    }

    // Read it back. A write nothing can observe is not a write.
    let after = convar::parse_features(&handle.exec("applejack_features", "test").await.unwrap());
    assert!(
        after.iter().any(|f| f.id == "ui.menu.admin" && f.enabled),
        "the feature stayed off: {after:#?}"
    );

    let after = convar::parse_settings(&handle.exec("applejack_settings", "test").await.unwrap());
    let arrest = after
        .iter()
        .find(|setting| setting.id == "crime.arrest_seconds")
        .unwrap();
    assert_eq!(arrest.value, "600");
    assert!(!arrest.is_default(), "it is now an override");

    // And it survives a round trip through the file format.
    let snapshot = convar::Snapshot {
        features: after_features(&handle).await,
        settings: after.clone(),
        ..Default::default()
    };
    let text = snapshot.to_toml().unwrap();
    assert_eq!(convar::Snapshot::parse(&text).unwrap(), snapshot);

    handle.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

/// After a stop the dashboard needs a reason, not an absence. The supervisor
/// used to end its task here, taking the roster, the resource history and the
/// restart button with it, and /api/status answered `"server": null`.
#[tokio::test]
async fn a_stopped_server_keeps_answering_and_says_how_it_ended() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path().join("logs/sbox-server.log"), &[]);
    config.supervisor.restart = cellar_core::lifecycle::RestartPolicy::Never;

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ServerReady { .. })
    })
    .await;

    handle.stop().await;
    wait_for(&mut events, Duration::from_secs(10), |e| {
        matches!(e, Event::ProcessExited { .. })
    })
    .await;

    let snapshot = handle
        .snapshot()
        .await
        .expect("the supervisor still answers with no server running");
    assert_eq!(snapshot.state, cellar_core::State::Stopped);
    assert!(snapshot.pid.is_none());

    let exit = snapshot.last_exit.expect("the exit is on the snapshot");
    assert_eq!(exit.code, Some(0));
    assert!(exit.graceful);

    // And the server can be started again from the same handle.
    handle.restart().await;
    wait_for(&mut events, Duration::from_secs(15), |e| {
        matches!(e, Event::ServerReady { .. })
    })
    .await;

    handle.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
}

async fn after_features(handle: &cellar_runtime::Handle) -> Vec<cellar_core::convar::Feature> {
    cellar_core::convar::parse_features(&handle.exec("applejack_features", "test").await.unwrap())
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
    handle.shutdown().await;

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

    handle.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
}

/// Never reporting ready is correct and is not enough on its own: `Starting`
/// forever looks exactly like a slow map load, and both observed causes are
/// permanent. The state has to say so.
#[tokio::test]
async fn a_server_that_never_becomes_ready_stops_claiming_to_be_starting() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path().join("logs/sbox-server.log"), &["--hang"]);
    config.supervisor.graceful_timeout_seconds = 1;
    config.supervisor.start_timeout_seconds = 1;

    let (supervisor, handle, control) = Supervisor::new(config);
    let mut events = handle.subscribe();
    let task = tokio::spawn(supervisor.run(control));

    let notice = wait_for(
        &mut events,
        Duration::from_secs(15),
        |e| matches!(e, Event::Unparsed { raw, .. } if raw.contains("has been starting for")),
    )
    .await;
    let Event::Unparsed { raw, .. } = notice else {
        panic!("wrong event")
    };
    assert!(raw.contains("ready pattern"), "{raw}");
    assert!(raw.contains("still running"), "{raw}");

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.state, cellar_core::State::Unhealthy);
    // Alive, untouched. The wrong ready pattern in front of a healthy server is
    // one of the two causes, and killing it would be the wrong answer to it.
    assert!(snapshot.pid.is_some());

    handle.shutdown().await;
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

    handle.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

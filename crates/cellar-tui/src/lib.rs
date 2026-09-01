//! The terminal dashboard.
//!
//! htop for a dedicated server: load, roster, live log, and a command line that
//! types straight into the game's console. It reads the same snapshot and the
//! same event stream the web UI does, so the two cannot disagree about what the
//! server is doing.

pub mod theme;
mod view;

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use cellar_core::event::{Event, Level};
use cellar_core::snapshot::Snapshot;
use cellar_runtime::Handle;
use crossterm::event::{self as term, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::broadcast;

/// How many log lines the pane keeps.
const SCROLLBACK: usize = 1000;

/// One line as the log pane holds it.
#[derive(Debug, Clone)]
pub struct Row {
    pub at: String,
    pub who: String,
    pub message: String,
    pub level: Level,
    /// Set for a line Cellar itself produced, so an operator can tell the
    /// supervisor's voice from the engine's.
    pub local: bool,
}

/// Everything the screen draws.
pub struct App {
    pub snapshot: Option<Snapshot>,
    /// What this screen is about, when it is not the only thing running.
    ///
    /// The TUI follows the primary instance and always has. On a one-server
    /// deployment that is the whole truth and naming it would be noise; on a
    /// two-server one it silently showed one of them with nothing on screen
    /// saying which, so a `quit` typed here went somewhere the operator had not
    /// chosen.
    pub instance: Option<String>,
    /// The gamemode's own name, when its profile gives one. The masthead said
    /// APPLEJACK to every gamemode before profiles existed.
    pub gamemode: Option<String>,
    pub rows: VecDeque<Row>,
    pub cpu: VecDeque<u64>,
    pub memory: VecDeque<u64>,
    pub input: String,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub follow: bool,
    pub scroll: usize,
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            snapshot: None,
            instance: None,
            gamemode: None,
            rows: VecDeque::with_capacity(SCROLLBACK),
            cpu: VecDeque::with_capacity(240),
            memory: VecDeque::with_capacity(240),
            input: String::new(),
            history: Vec::new(),
            history_index: None,
            follow: true,
            scroll: 0,
            should_quit: false,
        }
    }

    /// Fold an event into what the screen shows.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Log(line) => self.push(Row {
                at: line.at.format("%H:%M:%S").to_string(),
                who: line.logger.clone(),
                message: line.message.clone(),
                level: line.level,
                local: false,
            }),
            Event::PlayerJoined { name, steam_id } => self.push(Row {
                at: now(),
                who: "join".into(),
                message: format!("{name} [{steam_id}]"),
                level: Level::Info,
                local: true,
            }),
            Event::PlayerLeft { name, steam_id, .. } => self.push(Row {
                at: now(),
                who: "left".into(),
                message: format!("{name} [{steam_id}]"),
                level: Level::Info,
                local: true,
            }),
            Event::ProcessStarted { pid, .. } => self.push(Row {
                at: now(),
                who: "cellar".into(),
                message: format!("started, pid {pid}"),
                level: Level::Info,
                local: true,
            }),
            Event::ServerReady { .. } => self.push(Row {
                at: now(),
                who: "cellar".into(),
                message: "ready, accepting players".into(),
                level: Level::Info,
                local: true,
            }),
            Event::ProcessExited { code, graceful } => self.push(Row {
                at: now(),
                who: "cellar".into(),
                message: match code {
                    Some(code) if *graceful => format!("stopped cleanly, code {code}"),
                    Some(code) => format!("exited unexpectedly, code {code}"),
                    None => "killed by a signal".into(),
                },
                level: if *graceful { Level::Info } else { Level::Error },
                local: true,
            }),
            Event::Unparsed { raw, .. } => self.push(Row {
                at: now(),
                who: "?".into(),
                message: raw.clone(),
                level: Level::Info,
                local: false,
            }),
            Event::CommandReplied { reply, .. } => {
                for line in reply {
                    self.push(Row {
                        at: now(),
                        who: "reply".into(),
                        message: line.clone(),
                        level: Level::Info,
                        local: true,
                    });
                }
            }
            Event::Resources(sample) => {
                let cpu = if sample.cpu_core_count > 0 {
                    sample.cpu_percent_all_cores
                } else {
                    sample.cpu_percent
                };
                push_bounded(&mut self.cpu, cpu.max(0.0) as u64, 240);
                push_bounded(&mut self.memory, sample.memory_bytes / (1024 * 1024), 240);
            }
            Event::CommandDispatched { .. } | Event::Status(_) | Event::BridgeHealth { .. } => {}
        }
    }

    fn push(&mut self, row: Row) {
        if self.rows.len() == SCROLLBACK {
            self.rows.pop_front();
        }
        self.rows.push_back(row);

        // Scrolled back to read something? Stay there, and keep the offset
        // pointing at the same line as new ones arrive.
        if !self.follow {
            self.scroll = self.scroll.saturating_add(1);
        }
    }

    /// Handle a keypress. Returns a command to dispatch, if the key submitted one.
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Esc => {
                if self.input.is_empty() {
                    self.should_quit = true;
                } else {
                    self.input.clear();
                }
            }
            KeyCode::Enter => {
                let command = self.input.trim().to_owned();
                self.input.clear();
                self.history_index = None;
                if !command.is_empty() {
                    self.history.push(command.clone());
                    return Some(command);
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Up => self.recall(-1),
            KeyCode::Down => self.recall(1),
            KeyCode::PageUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_add(10);
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                if self.scroll == 0 {
                    self.follow = true;
                }
            }
            KeyCode::End => {
                self.scroll = 0;
                self.follow = true;
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
        None
    }

    fn recall(&mut self, direction: i32) {
        if self.history.is_empty() {
            return;
        }

        let last = self.history.len() - 1;
        self.history_index = match (self.history_index, direction) {
            (None, -1) => Some(last),
            (Some(0), -1) => Some(0),
            (Some(index), -1) => Some(index - 1),
            (Some(index), 1) if index >= last => None,
            (Some(index), 1) => Some(index + 1),
            (None, _) => None,
            (Some(index), _) => Some(index),
        };

        self.input = match self.history_index {
            Some(index) => self.history[index].clone(),
            None => String::new(),
        };
    }
}

fn push_bounded(series: &mut VecDeque<u64>, value: u64, limit: usize) {
    if series.len() == limit {
        series.pop_front();
    }
    series.push_back(value);
}

fn now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Run the dashboard until the operator quits.
///
/// The terminal is restored on every exit path, including a panic: a manager
/// that leaves the terminal in raw mode after a crash makes the next command
/// unreadable, which is the moment somebody most needs to type one.
/// Drive the terminal dashboard for one supervised server.
///
/// `instance` is `None` for a deployment with one server, where naming it would
/// be noise, and `Some(id)` when there are several and the screen has to say
/// which one it is about.
pub async fn run(
    handle: Handle,
    instance: Option<String>,
    gamemode: Option<String>,
) -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
        previous(info);
    }));

    let result = drive(
        &handle,
        instance,
        gamemode,
        Terminal::new(CrosstermBackend::new(io::stdout()))?,
    )
    .await;

    disable_raw_mode()?;
    crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;
    result
}

async fn drive<B: ratatui::backend::Backend>(
    handle: &Handle,
    instance: Option<String>,
    gamemode: Option<String>,
    mut terminal: Terminal<B>,
) -> io::Result<()> {
    let mut app = App::new();
    app.instance = instance;
    app.gamemode = gamemode;
    let mut events = handle.subscribe();
    let mut ticker = tokio::time::interval(Duration::from_millis(200));

    app.snapshot = handle.snapshot().await;

    loop {
        terminal.draw(|frame| view::draw(frame, &app))?;

        tokio::select! {
            received = events.recv() => match received {
                Ok(event) => app.apply(&event),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },

            _ = ticker.tick() => {
                app.snapshot = handle.snapshot().await;

                // Terminal input is polled rather than awaited: crossterm's
                // blocking read would hold this task and stop the event stream
                // draining, which shows up as a dashboard that only updates when
                // a key is pressed.
                while term::poll(Duration::from_millis(0))? {
                    if let TermEvent::Key(key) = term::read()?
                        && key.kind == KeyEventKind::Press
                        && let Some(command) = app.key(key.code, key.modifiers)
                    {
                        let _ = handle.exec(&command, "tui").await;
                    }
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn enter_submits_a_command_and_clears_the_line() {
        let mut app = App::new();
        for c in "applejack_features".chars() {
            app.key(KeyCode::Char(c), KeyModifiers::NONE);
        }

        assert_eq!(
            app.key(KeyCode::Enter, KeyModifiers::NONE).as_deref(),
            Some("applejack_features")
        );
        assert!(app.input.is_empty());
        assert_eq!(app.history, vec!["applejack_features"]);
    }

    #[test]
    fn an_empty_line_submits_nothing() {
        let mut app = App::new();
        assert!(app.key(KeyCode::Enter, KeyModifiers::NONE).is_none());
        assert!(app.history.is_empty());
    }

    #[test]
    fn history_walks_up_and_back_down_to_an_empty_line() {
        let mut app = App::new();
        app.history = vec!["first".into(), "second".into()];

        app.key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input, "second");

        app.key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input, "first");

        // Already at the oldest; staying there beats wrapping to the newest.
        app.key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input, "first");

        app.key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.input, "second");

        app.key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.input, "");
    }

    #[test]
    fn escape_clears_a_line_and_only_quits_when_it_is_already_empty() {
        let mut app = App::new();
        app.key(KeyCode::Char('x'), KeyModifiers::NONE);

        app.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.input.is_empty());
        assert!(!app.should_quit, "the first escape cleared the line");

        app.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_mid_command() {
        let mut app = App::new();
        app.key(KeyCode::Char('q'), KeyModifiers::NONE);
        app.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.should_quit);
    }

    /// A `q` typed into the command line must be a `q`, not a quit. This is the
    /// bug that makes a console unusable.
    #[test]
    fn typing_q_types_a_q() {
        let mut app = App::new();
        app.key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(app.input, "q");
        assert!(!app.should_quit);
    }

    #[test]
    fn scrollback_is_bounded_and_paging_stops_following() {
        let mut app = App::new();
        for index in 0..(SCROLLBACK + 100) {
            app.apply(&Event::Unparsed {
                raw: format!("line {index}"),
                origin: cellar_core::Origin::Console,
            });
        }
        assert_eq!(app.rows.len(), SCROLLBACK);

        app.key(KeyCode::PageUp, KeyModifiers::NONE);
        assert!(!app.follow);

        app.key(KeyCode::End, KeyModifiers::NONE);
        assert!(app.follow);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn a_crash_is_recorded_as_an_error_row() {
        let mut app = App::new();
        app.apply(&Event::ProcessExited {
            code: Some(1),
            graceful: false,
        });

        let row = app.rows.back().unwrap();
        assert_eq!(row.level, Level::Error);
        assert!(row.message.contains("unexpectedly"));
    }

    #[test]
    fn resource_samples_feed_the_sparklines() {
        let mut app = App::new();
        app.apply(&Event::Resources(cellar_core::event::ResourceSample {
            at: chrono::Utc::now(),
            cpu_percent: 141.0,
            cpu_percent_all_cores: 17.625,
            cpu_core_count: 8,
            memory_bytes: 3 * 1024 * 1024 * 1024,
            process_count: 2,
            host_cpu_percent: 24.0,
            host_memory_percent: 50.0,
            network_rx_bytes_per_sec: 0,
            network_tx_bytes_per_sec: 0,
        }));

        assert_eq!(app.cpu.back().copied(), Some(17));
        assert_eq!(app.memory.back().copied(), Some(3072));
    }
}

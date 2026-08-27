//! Drawing the dashboard.
//!
//! Layout is fixed rather than configurable: a status bar, two sparklines, the
//! roster, the log, and the command line. An operations screen earns its keep by
//! being in the same place every time.

use cellar_runtime::metrics::{format_bytes, format_uptime};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row as TableRow, Sparkline, Table};

use crate::App;
use crate::theme;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // status
            Constraint::Length(6), // load
            Constraint::Min(8),    // roster and log
            Constraint::Length(3), // command line
        ])
        .split(area);

    draw_status(frame, rows[0], app);
    draw_load(frame, rows[1], app);

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(rows[2]);

    draw_roster(frame, middle[0], app);
    draw_log(frame, middle[1], app);
    draw_prompt(frame, rows[3], app);
}

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(format!(" {title} "), theme::title()))
        .style(Style::default().bg(theme::panel()))
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled("★ ", theme::accent()),
        Span::styled(
            "APPLEJACK ",
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD),
        ),
    ];

    match &app.snapshot {
        Some(snapshot) => {
            spans.push(Span::styled(
                format!("● {} ", snapshot.state.as_str()),
                Style::default().fg(theme::state_colour(snapshot.state)),
            ));
            spans.push(Span::styled(
                format!("{} ", snapshot.hostname),
                theme::body(),
            ));
            spans.push(Span::styled(
                format!(
                    "· {}/{} players · up {} ",
                    snapshot.players.len(),
                    if snapshot.max_players == 0 {
                        "?".to_owned()
                    } else {
                        snapshot.max_players.to_string()
                    },
                    format_uptime(snapshot.uptime_seconds(chrono::Utc::now()))
                ),
                theme::dim(),
            ));

            if snapshot.bridge.enabled {
                let (label, style) = if snapshot.bridge.healthy {
                    ("bridge ok", Style::default().fg(theme::orchard()))
                } else {
                    ("bridge failing", Style::default().fg(theme::russet()))
                };
                spans.push(Span::styled(format!("· {label} "), style));
            }

            // A rising count means an engine update moved a log string and the
            // grammar needs revisiting. Worth a permanent place on the screen.
            if snapshot.unparsed_lines > 0 {
                spans.push(Span::styled(
                    format!("· {} unparsed ", snapshot.unparsed_lines),
                    theme::dim(),
                ));
            }
        }
        None => spans.push(Span::styled(
            "● connecting ",
            Style::default().fg(theme::frost()),
        )),
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(panel("dispatch")),
        area,
    );
}

fn draw_load(frame: &mut Frame, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let cpu: Vec<u64> = app.cpu.iter().copied().collect();
    let latest_cpu = cpu.last().copied().unwrap_or(0);
    frame.render_widget(
        Sparkline::default()
            .block(panel(&format!("cpu {latest_cpu}%")))
            .data(&cpu)
            .style(Style::default().fg(theme::azure())),
        columns[0],
    );

    let memory: Vec<u64> = app.memory.iter().copied().collect();
    let latest_memory = memory.last().copied().unwrap_or(0);
    frame.render_widget(
        Sparkline::default()
            .block(panel(&format!(
                "memory {}",
                format_bytes(latest_memory * 1024 * 1024)
            )))
            .data(&memory)
            .style(Style::default().fg(theme::frost())),
        columns[1],
    );
}

fn draw_roster(frame: &mut Frame, area: Rect, app: &App) {
    let now = chrono::Utc::now();

    let rows: Vec<TableRow> = app
        .snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .players
                .iter()
                .map(|player| {
                    TableRow::new(vec![
                        player.name.clone(),
                        format_uptime(player.connected_seconds(now)),
                    ])
                    .style(theme::body())
                })
                .collect()
        })
        .unwrap_or_default();

    let count = rows.len();
    let table = if rows.is_empty() {
        Table::new(
            vec![
                TableRow::new(vec!["nobody connected".to_owned(), String::new()])
                    .style(theme::dim()),
            ],
            [Constraint::Min(10), Constraint::Length(8)],
        )
    } else {
        Table::new(rows, [Constraint::Min(10), Constraint::Length(8)])
    };

    frame.render_widget(
        table
            .header(TableRow::new(vec!["player", "for"]).style(theme::title()))
            .block(panel(&format!("roster ({count})"))),
        area,
    );
}

fn draw_log(frame: &mut Frame, area: Rect, app: &App) {
    // Two for the borders.
    let height = area.height.saturating_sub(2) as usize;
    let total = app.rows.len();

    let end = total.saturating_sub(app.scroll);
    let start = end.saturating_sub(height);

    let lines: Vec<Line> = app
        .rows
        .iter()
        .skip(start)
        .take(end - start)
        .map(|row| {
            let message = Style::default().fg(theme::log_colour(
                row.level,
                &row.who,
                &row.message,
                row.local,
            ));

            Line::from(vec![
                Span::styled(format!("{} ", row.at), theme::dim()),
                Span::styled(
                    format!("{:<9}", truncate(&row.who, 9)),
                    Style::default().fg(theme::frost()),
                ),
                Span::styled(row.message.clone(), message),
            ])
        })
        .collect();

    let title = if app.follow {
        "console".to_owned()
    } else {
        format!("console (paused, {} back)", app.scroll)
    };

    frame.render_widget(Paragraph::new(lines).block(panel(&title)), area);
}

fn draw_prompt(frame: &mut Frame, area: Rect, app: &App) {
    let line = Line::from(vec![
        Span::styled("> ", theme::accent()),
        Span::styled(app.input.clone(), theme::body()),
        Span::styled("█", theme::accent()),
    ]);

    frame.render_widget(
        Paragraph::new(line).block(panel("command  ·  enter send  ·  pgup scroll  ·  esc quit")),
        area,
    );
}

/// Cut a string to fit, in characters rather than bytes.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    text.chars().take(width).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn it_draws_before_the_first_snapshot_arrives() {
        let screen = render(&App::new(), 100, 30);
        assert!(screen.contains("APPLEJACK"));
        assert!(screen.contains("connecting"));
    }

    #[test]
    fn a_narrow_terminal_does_not_panic() {
        // The failure this guards: a layout that underflows on a small window
        // and takes the whole dashboard down with it.
        for (width, height) in [(20u16, 10u16), (40, 12), (200, 60), (30, 20)] {
            render(&App::new(), width, height);
        }
    }

    #[test]
    fn the_log_pane_shows_the_newest_lines_when_following() {
        let mut app = App::new();
        for index in 0..200 {
            app.apply(&cellar_core::Event::Unparsed {
                raw: format!("line-{index}"),
                origin: cellar_core::Origin::Console,
            });
        }

        let screen = render(&app, 100, 30);
        assert!(screen.contains("line-199"), "the newest line is on screen");
        assert!(!screen.contains("line-0 "), "the oldest is not");
    }

    #[test]
    fn the_command_line_shows_what_is_typed() {
        let mut app = App::new();
        app.input = "applejack_features".into();

        let screen = render(&app, 100, 30);
        assert!(screen.contains("applejack_features"));
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("★★★★★", 2), "★★");
        assert_eq!(truncate("ab", 5), "ab");
    }
}

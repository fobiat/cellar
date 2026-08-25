//! The dedicated server console's status bar, rendered and parsed.
//!
//! Every rule here is taken from `engine/Launcher/SboxServer/DedicatedServerConsole.cs`
//! (`UpdateStatus`) and `ConsoleInput.cs` (`RedrawInputLine`), so [`render`] is a
//! transcription of the engine rather than an impression of it, and the parser is
//! tested against that transcription.

use crate::event::StatusBar;

/// Half of the bar. The engine draws the two halves as two separate console
/// lines (`SetStatus(1, lineA)` and `SetStatus(2, lineB)`), so a reader sees one
/// at a time and has to merge them.
#[derive(Debug, Clone, PartialEq)]
pub enum Fragment {
    /// `{name} ({players}/{max}) [{h:mm:ss}]` … `Network {n}ms`
    Head {
        hostname: String,
        players: u32,
        max_players: u32,
        uptime_seconds: u64,
        network_ms: Option<f32>,
    },
    /// `Physics {n}ms, NavMesh {n}ms, Animation {n}ms` … `Update {n}ms`
    Timings {
        physics_ms: Option<f32>,
        navmesh_ms: Option<f32>,
        animation_ms: Option<f32>,
        update_ms: Option<f32>,
    },
}

impl Fragment {
    /// Fold this half into a bar, leaving the other half's fields alone.
    pub fn apply(self, bar: &mut StatusBar) {
        match self {
            Fragment::Head {
                hostname,
                players,
                max_players,
                uptime_seconds,
                network_ms,
            } => {
                bar.hostname = hostname;
                bar.players = players;
                bar.max_players = max_players;
                bar.uptime_seconds = uptime_seconds;
                bar.network_ms = network_ms;
            }
            Fragment::Timings {
                physics_ms,
                navmesh_ms,
                animation_ms,
                update_ms,
            } => {
                bar.physics_ms = physics_ms;
                bar.navmesh_ms = navmesh_ms;
                bar.animation_ms = animation_ms;
                bar.update_ms = update_ms;
            }
        }
    }
}

/// Recognise either half of the status bar.
pub fn parse(text: &str) -> Option<Fragment> {
    let text = text.trim_end();

    if let Some((players, max_players, counter_start)) = find_player_counter(text) {
        let tail = &text[counter_start..];
        // The clock is required, not optional. `UpdateStatus` always renders it,
        // and without it an ordinary log line like `Loading map (1/3) stage`
        // matches on the counter alone and overwrites the real bar.
        if let Some(uptime_seconds) = find_bracketed_clock(tail) {
            return Some(Fragment::Head {
                hostname: text[..counter_start].trim_end().to_owned(),
                players,
                max_players,
                uptime_seconds,
                network_ms: find_timing(tail, "Network"),
            });
        }
    }

    // Line B carries no counter and no brackets. Require the label that always
    // leads it, so an ordinary log line mentioning physics is not mistaken for
    // the bar.
    let physics = find_timing(text, "Physics")?;
    Some(Fragment::Timings {
        physics_ms: Some(physics),
        navmesh_ms: find_timing(text, "NavMesh"),
        animation_ms: find_timing(text, "Animation"),
        update_ms: find_timing(text, "Update"),
    })
}

/// True for the padded blank lines `RedrawInputLine` writes around the bar.
///
/// It emits one blank input line plus `statusText[0]`, which
/// `DedicatedServerConsole` never sets, both right-padded to the console width.
/// They carry nothing and there are two of them every half second.
pub fn is_blank_chrome(text: &str) -> bool {
    text.trim().is_empty()
}

/// The line `ConsoleInput.OnEnter` writes immediately before it dispatches.
///
/// `Console.WriteLine( "> " + inputString )` runs before `OnInputText`, so this
/// marks the exact start of a command's output with no timing guess involved.
pub fn parse_command_echo(text: &str) -> Option<&str> {
    text.trim_end().strip_prefix("> ")
}

/// Seconds of uptime from the bar's clock, correcting the engine's rounding.
///
/// `UpdateStatus` renders the hour as `{TotalHours:n0}`, which **rounds** rather
/// than truncates, while the minutes and seconds come from `ToString("mm\:ss")`
/// and are exact. So a server up for 40 minutes renders `[1:40:00]`, an hour
/// ahead of the truth, and stays wrong for half of every hour.
///
/// The error is exactly invertible: rendered = floor_hours + (minutes >= 30).
/// Subtracting that back recovers the real hour, so Cellar reports true uptime
/// from a bar the engine renders incorrectly.
fn clock_to_seconds(hours: u64, minutes: u64, seconds: u64) -> u64 {
    let corrected = hours.saturating_sub(u64::from(minutes >= 30));
    corrected * 3600 + minutes * 60 + seconds
}

fn find_player_counter(text: &str) -> Option<(u32, u32, usize)> {
    let mut search = 0usize;
    while let Some(open) = text[search..].find('(') {
        let open = search + open;
        let Some(close) = text[open..].find(')') else {
            break;
        };
        let close = open + close;
        let inner = &text[open + 1..close];
        if let Some((left, right)) = inner.split_once('/')
            && let (Ok(players), Ok(max)) =
                (left.trim().parse::<u32>(), right.trim().parse::<u32>())
        {
            return Some((players, max, open));
        }
        search = close + 1;
    }
    None
}

fn find_bracketed_clock(text: &str) -> Option<u64> {
    let open = text.find('[')?;
    let close = text[open..].find(']')? + open;
    let mut parts = text[open + 1..close].split(':');

    // `n0` inserts group separators, so a server up 1000 hours renders `1,000`.
    let hours: u64 = parts.next()?.trim().replace(',', "").parse().ok()?;
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    Some(clock_to_seconds(hours, minutes, seconds))
}

/// `{Label} {n}.{nn}ms`, with the label matched case-insensitively and the
/// number read by scanning forward rather than by column offset.
fn find_timing(text: &str, label: &str) -> Option<f32> {
    let lowered = text.to_ascii_lowercase();
    let label_lower = label.to_ascii_lowercase();
    let at = lowered.find(&label_lower)?;

    let rest = &text[at + label.len()..];
    // Tolerate a colon the engine does not currently write, so a future
    // `Network: 1.00ms` still reads.
    let rest = rest.trim_start().trim_start_matches(':').trim_start();

    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

/// Render the two status lines exactly as the engine does.
///
/// Mirrors `UpdateStatus` including the `PadLeft`/`Substring` right-alignment
/// and `RedrawInputLine`'s `PadRight`, so a fixture built from this is a
/// fixture of the engine's output. `width` is the engine's `lineWidth`, which is
/// `Console.BufferWidth - 1`.
pub fn render(bar: &StatusBar, width: usize) -> [String; 2] {
    let hours = bar.uptime_seconds / 3600;
    let minutes = (bar.uptime_seconds % 3600) / 60;
    let seconds = bar.uptime_seconds % 60;

    // The engine rounds the hour and Cellar corrects it on the way back in, so
    // to render what the engine renders the rounding has to be reintroduced.
    let rendered_hours = hours + u64::from(minutes >= 30);

    let uptime = format!(
        "{}:{minutes:02}:{seconds:02}",
        group_separated(rendered_hours)
    );
    let top_left = format!(
        "{} ({}/{}) [{uptime}]",
        bar.hostname, bar.players, bar.max_players
    );
    let top_right = format!("Network {:.2}ms", bar.network_ms.unwrap_or(0.0));

    let bottom_left = format!(
        "Physics {:.2}ms, NavMesh {:.2}ms, Animation {:.2}ms",
        bar.physics_ms.unwrap_or(0.0),
        bar.navmesh_ms.unwrap_or(0.0),
        bar.animation_ms.unwrap_or(0.0),
    );
    let bottom_right = format!("Update {:.2}ms", bar.update_ms.unwrap_or(0.0));

    [
        pad_right(&justify(&top_left, &top_right, width), width),
        pad_right(&justify(&bottom_left, &bottom_right, width), width),
    ]
}

/// `right.PadLeft(width)`, then `left` overwritten across the front of it.
fn justify(left: &str, right: &str, width: usize) -> String {
    let padded_right = pad_left(right, width);
    let left_len = left.chars().count();
    if left_len < padded_right.chars().count() {
        let tail: String = padded_right.chars().skip(left_len).collect();
        format!("{left}{tail}")
    } else {
        left.to_owned()
    }
}

fn pad_left(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_owned();
    }
    format!("{}{text}", " ".repeat(width - len))
}

fn pad_right(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_owned();
    }
    format!("{text}{}", " ".repeat(width - len))
}

/// .NET's `n0`: thousands separated, no decimals.
fn group_separated(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn bar() -> StatusBar {
        StatusBar {
            hostname: "AppleJackRP".into(),
            players: 3,
            max_players: 64,
            uptime_seconds: 3661,
            network_ms: Some(0.42),
            physics_ms: Some(1.10),
            navmesh_ms: Some(0.05),
            animation_ms: Some(0.31),
            update_ms: Some(3.75),
        }
    }

    #[test]
    fn renders_the_engines_two_lines() {
        let [a, b] = render(&bar(), 100);

        // The right half sits flush against `width`, which is what the
        // engine's `PadLeft(width)` then overwrite-from-the-left produces.
        assert!(a.starts_with("AppleJackRP (3/64) [1:01:01] "));
        assert!(a.ends_with("Network 0.42ms"));
        assert!(b.starts_with("Physics 1.10ms, NavMesh 0.05ms, Animation 0.31ms "));
        assert!(b.ends_with("Update 3.75ms"));
        // RedrawInputLine right-pads every status line to the console width.
        assert_eq!(a.chars().count(), 100);
        assert_eq!(b.chars().count(), 100);
    }

    #[test]
    fn a_rendered_bar_parses_back_to_itself() {
        let original = bar();
        let [a, b] = render(&original, 100);

        let mut round_tripped = StatusBar::default();
        parse(&a).unwrap().apply(&mut round_tripped);
        parse(&b).unwrap().apply(&mut round_tripped);

        assert_eq!(round_tripped, original);
    }

    #[test]
    fn line_b_parses_without_a_player_counter() {
        // The regression this module exists for: line B has no `(n/m)` and no
        // clock, so a parser anchored on the counter rejected it and every
        // half-second redraw landed in the unparsed counter.
        let [_, b] = render(&bar(), 100);
        let fragment = parse(&b).unwrap();

        assert_eq!(
            fragment,
            Fragment::Timings {
                physics_ms: Some(1.10),
                navmesh_ms: Some(0.05),
                animation_ms: Some(0.31),
                update_ms: Some(3.75),
            }
        );
    }

    #[test]
    fn corrects_the_engines_rounded_hour() {
        // 40 minutes of uptime renders as `[1:40:00]` because `TotalHours:n0`
        // rounds 0.67 up to 1. Reading it literally is an hour out.
        let mut forty_minutes = bar();
        forty_minutes.uptime_seconds = 40 * 60;
        let [a, _] = render(&forty_minutes, 100);
        assert!(a.contains("[1:40:00]"), "engine renders the rounded hour");

        let mut parsed = StatusBar::default();
        parse(&a).unwrap().apply(&mut parsed);
        assert_eq!(parsed.uptime_seconds, 40 * 60, "Cellar reports the truth");
    }

    #[test]
    fn under_thirty_minutes_is_not_corrected() {
        let mut twenty_nine = bar();
        twenty_nine.uptime_seconds = 29 * 60 + 59;
        let [a, _] = render(&twenty_nine, 100);
        assert!(a.contains("[0:29:59]"));

        let mut parsed = StatusBar::default();
        parse(&a).unwrap().apply(&mut parsed);
        assert_eq!(parsed.uptime_seconds, 29 * 60 + 59);
    }

    #[test]
    fn reads_a_grouped_hour() {
        // `n0` writes 1000 hours as `1,000`, which a plain integer parse rejects.
        let mut long_lived = bar();
        long_lived.uptime_seconds = 1000 * 3600 + 5 * 60;
        let [a, _] = render(&long_lived, 120);
        assert!(a.contains("[1,000:05:00]"), "got {a}");

        let mut parsed = StatusBar::default();
        parse(&a).unwrap().apply(&mut parsed);
        assert_eq!(parsed.uptime_seconds, 1000 * 3600 + 5 * 60);
    }

    #[test]
    fn a_hostname_with_brackets_and_a_counter_does_not_derail_it() {
        let mut awkward = bar();
        awkward.hostname = "Kyle's (test) [beta] server".into();
        let [a, _] = render(&awkward, 120);

        let mut parsed = StatusBar::default();
        parse(&a).unwrap().apply(&mut parsed);
        // `(test)` holds no `/`, so the counter search walks past it.
        assert_eq!(parsed.players, 3);
        assert_eq!(parsed.max_players, 64);
        assert_eq!(parsed.hostname, "Kyle's (test) [beta] server");
    }

    #[test]
    fn a_narrow_console_truncates_the_right_half() {
        // `topLeft.Length < lineA.Length` is false once the left half fills the
        // width, and the engine then drops the right half entirely.
        let [a, _] = render(&bar(), 10);
        assert_eq!(a, "AppleJackRP (3/64) [1:01:01]");
        assert!(!a.contains("Network"));
    }

    #[test]
    fn blank_chrome_is_recognised() {
        assert!(is_blank_chrome(&" ".repeat(120)));
        assert!(is_blank_chrome(""));
        assert!(!is_blank_chrome(&render(&bar(), 100)[0]));
    }

    #[test]
    fn the_command_echo_is_recognised() {
        assert_eq!(
            parse_command_echo("> applejack_features        "),
            Some("applejack_features")
        );
        assert_eq!(parse_command_echo("applejack_features"), None);
        // A log line that happens to start with a quote marker is not an echo.
        assert_eq!(parse_command_echo(">no space"), None);
    }

    #[test]
    fn an_ordinary_log_line_is_not_a_status_bar() {
        assert!(parse("Physics system initialised").is_none());
        assert!(parse("Player connected").is_none());
        // A counter without the bar's clock is not the bar.
        assert!(parse("Loading map (1/3) stage").is_none());
        assert!(parse("Compiled 12 shaders (3/4)").is_none());
    }
}

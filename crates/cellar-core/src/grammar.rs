//! Turning s&box output into events.
//!
//! Two input formats, because the engine writes two. Both are parsed here, as
//! functions over `&str`, so the whole vocabulary can be tested against a
//! fixture corpus with no game server anywhere near it.
//!
//! Formats are taken from engine source rather than guessed:
//!
//! - Log file, `Sandbox.System/Logging/Logging.cs`:
//!   `yyyy/MM/dd HH:mm:ss.ffff` TAB `[logger] message` TAB `exception`
//! - Console, `Sandbox.System/Logging/GameLog.cs`:
//!   `hh:mm:ss ` then the logger name padded or truncated to exactly 8
//!   characters, then a space, then the message. Continuation lines of a
//!   multi-line message are indented by 17 spaces, which is 9 + 8.
//!
//! Neither format carries a level. The file layout has no `${level}` field, and
//! on a pseudo-terminal the engine's stdout and stderr are the same stream, so
//! the "errors go to stderr" split is gone too. Level is therefore inferred,
//! and the rule is written down in [`infer_level`] rather than left implicit.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

use crate::event::{Event, LeaveReason, Level, LogLine, Origin, SteamId};
use crate::statusbar;

/// Width the console pads or truncates a logger name to.
const CONSOLE_LOGGER_WIDTH: usize = 8;

/// `hh:mm:ss ` is 9 characters, plus the 8 above.
const CONSOLE_CONTINUATION_INDENT: usize = 17;

/// Engine line announcing a connection, `NetworkSystem.Handshake.cs`.
const JOINED_SUFFIX: &str = " is connected";

/// Engine line announcing a disconnection, `NetworkSystem.Connections.cs`.
const LEFT_SUFFIX: &str = " disconnected";

/// The readiness line Cellar watches for by default.
///
/// AppleJackRP's `Code/NetworkBootstrap.cs` logs this once the lobby exists and
/// the session will accept joins, which is the earliest honest "serving" signal
/// available without a query protocol. Configurable, because a different
/// gamemode logs something different.
pub const DEFAULT_READY_PATTERN: &str = "Lobby created - session is joinable";

/// One line, already stripped of terminal control sequences, with the channel
/// it arrived on.
#[derive(Debug, Clone, Copy)]
pub struct Line<'a> {
    pub text: &'a str,
    pub origin: Origin,
}

impl<'a> Line<'a> {
    pub fn console(text: &'a str) -> Self {
        Self {
            text,
            origin: Origin::Console,
        }
    }

    pub fn log_file(text: &'a str) -> Self {
        Self {
            text,
            origin: Origin::LogFile,
        }
    }
}

/// A line broken into its parts, before it is classified into an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub at: Option<DateTime<Utc>>,
    pub logger: Option<String>,
    pub message: String,
    /// Present only in the log file's third field.
    pub exception: Option<String>,
    /// True when this is a continuation of the previous message.
    pub continuation: bool,
}

/// Split a raw line into timestamp, logger and message.
///
/// Returns `None` only for an empty line. A line that matches no known shape
/// still comes back, as a bare message, so the caller can count it rather than
/// drop it.
pub fn parse_line(line: Line<'_>) -> Option<Parsed> {
    let text = line.text.trim_end_matches(['\r', '\n']);
    if text.trim().is_empty() {
        return None;
    }

    match line.origin {
        Origin::LogFile => Some(parse_log_file_line(text)),
        Origin::Console => Some(parse_console_line(text)),
        Origin::Cellar => Some(Parsed {
            at: Some(Utc::now()),
            logger: Some("cellar".to_owned()),
            message: text.to_owned(),
            exception: None,
            continuation: false,
        }),
    }
}

fn parse_log_file_line(text: &str) -> Parsed {
    let mut fields = text.splitn(3, '\t');
    let stamp = fields.next().unwrap_or_default();
    let body = fields.next().unwrap_or_default();
    let exception = fields.next().unwrap_or_default();

    let at = parse_file_timestamp(stamp);

    // A line with no tab at all is not this format. Rather than guess, hand the
    // whole thing back as the message and let the caller count it unparsed.
    if at.is_none() && body.is_empty() {
        return Parsed {
            at: None,
            logger: None,
            message: text.to_owned(),
            exception: None,
            continuation: text.starts_with(' '),
        };
    }

    let (logger, message) = split_bracketed_logger(body);

    Parsed {
        at,
        logger,
        message,
        exception: (!exception.trim().is_empty()).then(|| exception.trim().to_owned()),
        continuation: false,
    }
}

// `[Identity] Kyle joined` -> ("Identity", "Kyle joined").
fn split_bracketed_logger(body: &str) -> (Option<String>, String) {
    let body = body.trim_start();
    if !body.starts_with('[') {
        return (None, body.to_owned());
    }

    match body.find(']') {
        Some(close) => {
            let logger = body[1..close].trim().to_owned();
            let message = body[close + 1..]
                .strip_prefix(' ')
                .unwrap_or(&body[close + 1..]);
            (Some(logger), message.to_owned())
        }
        None => (None, body.to_owned()),
    }
}

fn parse_console_line(text: &str) -> Parsed {
    if text.len() >= CONSOLE_CONTINUATION_INDENT
        && text[..CONSOLE_CONTINUATION_INDENT]
            .bytes()
            .all(|b| b == b' ')
    {
        return Parsed {
            at: None,
            logger: None,
            message: text[CONSOLE_CONTINUATION_INDENT..].to_owned(),
            exception: None,
            continuation: true,
        };
    }

    // `hh:mm:ss ` then exactly eight characters of logger, then a space.
    let looks_stamped = text.len() > CONSOLE_CONTINUATION_INDENT
        && text.as_bytes().get(2) == Some(&b':')
        && text.as_bytes().get(5) == Some(&b':')
        && text.as_bytes().get(8) == Some(&b' ');

    if !looks_stamped {
        return Parsed {
            at: None,
            logger: None,
            message: text.to_owned(),
            exception: None,
            continuation: false,
        };
    }

    let logger = text[9..9 + CONSOLE_LOGGER_WIDTH].trim_end().to_owned();
    let message = text[CONSOLE_CONTINUATION_INDENT..]
        .strip_prefix(' ')
        .unwrap_or("")
        .to_owned();

    Parsed {
        at: None, // 12-hour and undated; the file channel is the one to trust for time.
        logger: (!logger.is_empty()).then_some(logger),
        message,
        exception: None,
        continuation: false,
    }
}

fn parse_file_timestamp(stamp: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(stamp.trim(), "%Y/%m/%d %H:%M:%S%.f").ok()?;
    // The engine writes local time with no offset. Treating it as UTC would
    // shift every event by the host's offset, so this uses the local zone and
    // falls back to UTC only when the local time is ambiguous across a DST fold.
    match chrono::Local.from_local_datetime(&naive).single() {
        Some(local) => Some(local.with_timezone(&Utc)),
        None => Some(Utc.from_utc_datetime(&naive)),
    }
}

/// The level rule, stated once.
///
/// Neither channel carries a level, so this is inference and not fact: a line
/// with an exception attached is an error, and everything else is info. Cellar
/// never claims more than that, and the web UI labels the column accordingly.
pub fn infer_level(parsed: &Parsed) -> Level {
    if parsed.exception.is_some() {
        Level::Error
    } else {
        Level::Info
    }
}

/// Classify a parsed line into an event.
pub fn classify(parsed: &Parsed, origin: Origin, ready_pattern: &str) -> Event {
    let message = parsed.message.as_str();

    if let Some((steam_id, name)) = parse_join(message) {
        return Event::PlayerJoined { steam_id, name };
    }

    if let Some((steam_id, name)) = parse_leave(message) {
        return Event::PlayerLeft {
            steam_id,
            name,
            reason: LeaveReason::Disconnected,
        };
    }

    if let Some((steam_id, name, reason)) = parse_kick(message) {
        return Event::PlayerLeft {
            steam_id,
            name,
            reason: LeaveReason::Kicked { reason },
        };
    }

    if !ready_pattern.is_empty() && message.contains(ready_pattern) {
        return Event::ServerReady {
            hostname: None,
            map: None,
        };
    }

    match (&parsed.logger, parsed.at) {
        (Some(logger), _) => Event::Log(LogLine {
            at: parsed.at.unwrap_or_else(Utc::now),
            level: infer_level(parsed),
            logger: logger.clone(),
            message: parsed.message.clone(),
            origin,
        }),
        // No logger means nothing recognised the shape. Count it.
        (None, _) => Event::Unparsed {
            raw: parsed.message.clone(),
            origin,
        },
    }
}

/// `{name} [{steamid}] is connected`, anchored at the end.
///
/// Anchored deliberately. A Steam display name is chosen by the account holder
/// and may contain `[`, `]` and spaces, so a name reading
/// `Bob [76561198000000000] is connected` is legal and will appear inside a real
/// join line. Searching from the left finds the forged id; searching from the
/// right finds the engine's own.
pub fn parse_join(message: &str) -> Option<(SteamId, String)> {
    let head = message.strip_suffix(JOINED_SUFFIX)?;
    split_trailing_steam_id(head)
}

/// `{name} [{steamid}] disconnected`, anchored the same way.
pub fn parse_leave(message: &str) -> Option<(SteamId, String)> {
    let head = message.strip_suffix(LEFT_SUFFIX)?;
    split_trailing_steam_id(head)
}

/// `Kicking {name} [{steamid}] - {reason}` and the `Kicked` past tense.
pub fn parse_kick(message: &str) -> Option<(SteamId, String, String)> {
    let rest = message
        .strip_prefix("Kicking ")
        .or_else(|| message.strip_prefix("Kicked "))?;

    // The reason follows the closing bracket, so the id is not at the end here.
    // Take the last `]` and require what precedes it to be a bracketed id.
    let close = rest.rfind(']')?;
    let (head, tail) = rest.split_at(close + 1);
    let (steam_id, name) = split_trailing_steam_id(head)?;

    let reason = tail.trim_start().trim_start_matches('-').trim().to_owned();

    Some((steam_id, name, reason))
}

// Split `some name [76561198000000000]` into the id and the name before it.
fn split_trailing_steam_id(head: &str) -> Option<(SteamId, String)> {
    let head = head.strip_suffix(']')?;
    let open = head.rfind(" [")?;
    let digits = &head[open + 2..];

    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let steam_id: SteamId = digits.parse().ok()?;
    Some((steam_id, head[..open].to_owned()))
}

/// Parse either half of the dedicated console's status bar.
///
/// The engine draws it as two lines, so this answers a [`statusbar::Fragment`]
/// and the caller merges. See [`crate::statusbar`] for the rendering it is
/// anchored to.
pub fn parse_status_fragment(text: &str) -> Option<statusbar::Fragment> {
    statusbar::parse(text)
}

// `Network: 1.23ms`, `network 1.23`, `Network=1.23ms` all read the same.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_real_log_file_line() {
        let raw = "2026/08/25 14:02:11.1234\t[Identity] Kyle active with 2 character(s)\t";
        let parsed = parse_line(Line::log_file(raw)).unwrap();
        assert_eq!(parsed.logger.as_deref(), Some("Identity"));
        assert_eq!(parsed.message, "Kyle active with 2 character(s)");
        assert!(parsed.exception.is_none());
        assert!(parsed.at.is_some());
    }

    #[test]
    fn an_exception_field_makes_it_an_error() {
        let raw = "2026/08/25 14:02:11.1234\t[Storage] write failed\tSystem.Net.Http.HttpRequestException: boom";
        let parsed = parse_line(Line::log_file(raw)).unwrap();
        assert_eq!(infer_level(&parsed), Level::Error);
        assert!(parsed.exception.unwrap().contains("HttpRequestException"));
    }

    #[test]
    fn reads_a_console_line_with_its_padded_logger() {
        // "02:04:11 " is 9 chars, "Identity" is exactly 8, then a space.
        let raw = "02:04:11 Identity Kyle joined";
        let parsed = parse_line(Line::console(raw)).unwrap();
        assert_eq!(parsed.logger.as_deref(), Some("Identity"));
        assert_eq!(parsed.message, "Kyle joined");
    }

    #[test]
    fn a_short_logger_is_padded_and_trimmed_back() {
        // "Chat" padded to 8 becomes "Chat    ".
        let raw = "02:04:11 Chat     hello there";
        let parsed = parse_line(Line::console(raw)).unwrap();
        assert_eq!(parsed.logger.as_deref(), Some("Chat"));
        assert_eq!(parsed.message, "hello there");
    }

    #[test]
    fn a_continuation_line_is_marked_not_dropped() {
        let raw = format!("{}second line of a stack trace", " ".repeat(17));
        let parsed = parse_line(Line::console(&raw)).unwrap();
        assert!(parsed.continuation);
        assert_eq!(parsed.message, "second line of a stack trace");
    }

    #[test]
    fn parses_join_and_leave() {
        assert_eq!(
            parse_join("Kyle [76561198000000000] is connected"),
            Some((76561198000000000, "Kyle".to_owned()))
        );
        assert_eq!(
            parse_leave("Kyle [76561198000000000] disconnected"),
            Some((76561198000000000, "Kyle".to_owned()))
        );
    }

    /// The reason the parser anchors at the end rather than searching forwards.
    #[test]
    fn a_forged_name_cannot_steal_the_steam_id() {
        let forged = "Bob [76561198000000000] is connected";
        let line = format!("{forged} [76561198000000001] is connected");

        let (steam_id, name) = parse_join(&line).unwrap();
        assert_eq!(
            steam_id, 76561198000000001,
            "the engine's id, not the forged one"
        );
        assert_eq!(name, forged);
    }

    #[test]
    fn a_name_full_of_brackets_still_parses() {
        let (steam_id, name) = parse_join("[[]] ][ [x] [76561198000000000] is connected").unwrap();
        assert_eq!(steam_id, 76561198000000000);
        assert_eq!(name, "[[]] ][ [x]");
    }

    #[test]
    fn a_line_that_only_looks_like_a_join_is_refused() {
        assert!(parse_join("someone is connected").is_none());
        assert!(parse_join("Kyle [not-a-number] is connected").is_none());
        assert!(parse_join("Kyle [] is connected").is_none());
        // A bare bracket with no preceding space is not the engine's shape.
        assert!(parse_join("Kyle[76561198000000000] is connected").is_none());
    }

    #[test]
    fn parses_a_kick_with_its_reason() {
        let (steam_id, name, reason) =
            parse_kick("Kicking Kyle [76561198000000000] - not friends with host").unwrap();
        assert_eq!(steam_id, 76561198000000000);
        assert_eq!(name, "Kyle");
        assert_eq!(reason, "not friends with host");
    }

    #[test]
    fn classify_recognises_readiness() {
        let parsed = Parsed {
            at: None,
            logger: Some("Bootstra".to_owned()),
            message: DEFAULT_READY_PATTERN.to_owned(),
            exception: None,
            continuation: false,
        };
        assert!(matches!(
            classify(&parsed, Origin::Console, DEFAULT_READY_PATTERN),
            Event::ServerReady { .. }
        ));
    }

    #[test]
    fn an_unrecognised_line_is_counted_not_dropped() {
        let parsed = parse_line(Line::console("some bare engine chatter")).unwrap();
        assert!(matches!(
            classify(&parsed, Origin::Console, DEFAULT_READY_PATTERN),
            Event::Unparsed { .. }
        ));
    }

    #[test]
    fn status_lines_are_recognised_from_either_half() {
        // Both halves, as `DedicatedServerConsole` actually renders them. The
        // format itself is tested in `crate::statusbar`; this only proves the
        // grammar hands off to it.
        let head = parse_status_fragment("AppleJackRP Dev (7/64) [4:12:33]     Network 0.42ms");
        assert!(matches!(
            head,
            Some(statusbar::Fragment::Head { players: 7, .. })
        ));

        let timings = parse_status_fragment(
            "Physics 1.10ms, NavMesh 0.05ms, Animation 0.31ms   Update 3.75ms",
        );
        assert!(matches!(timings, Some(statusbar::Fragment::Timings { .. })));

        assert!(parse_status_fragment("Loading assets for the map").is_none());
    }
}

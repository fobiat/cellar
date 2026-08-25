//! Making a pseudo-terminal stream readable.
//!
//! The dedicated console is an interactive overlay: it colours log lines and it
//! redraws a status bar in place using cursor movement and carriage returns. A
//! supervisor reading the pty gets all of that mixed into the log text.
//!
//! Rather than track cursor state (which needs a full terminal emulator to do
//! correctly), this strips control sequences and then lets the caller identify
//! the status bar by its *shape*, via `grammar::parse_status_bar`. That is both
//! simpler and more robust: it survives the engine changing how it positions the
//! bar, and it fails by treating a status line as a log line rather than by
//! losing output.

/// Strip ANSI escape sequences from a chunk of terminal output.
///
/// Handles the two forms the engine's colouring produces, CSI (`ESC [ ... cmd`)
/// and OSC (`ESC ] ... BEL` or `ESC ] ... ESC \`), plus two-character escapes.
pub fn strip_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }

        match chars.next() {
            Some('[') => {
                // CSI: parameter and intermediate bytes, then one final byte.
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: runs until BEL or ST.
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Anything else is a two-character escape; both are consumed.
            Some(_) | None => {}
        }
    }

    out
}

/// Reduce in-place redraws to what a reader would finally see.
///
/// A status bar redrawn with `\r` leaves several versions of itself in one
/// physical line. Only the last one was ever visible, so that is the one kept.
pub fn collapse_carriage_returns(line: &str) -> &str {
    match line.rfind('\r') {
        Some(at) => &line[at + 1..],
        None => line,
    }
}

/// Accumulates pty bytes and yields whole, cleaned lines.
///
/// A pty read boundary lands anywhere, including inside a UTF-8 sequence and
/// inside an escape sequence, so partial input is held rather than parsed.
#[derive(Debug, Default)]
pub struct LineAssembler {
    buffer: Vec<u8>,
}

impl LineAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes; get back every complete line they finished.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);

        let mut lines = Vec::new();
        while let Some(at) = self.buffer.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buffer.drain(..=at).collect();
            let text = String::from_utf8_lossy(&raw);
            let cleaned = clean(&text);
            if !cleaned.trim().is_empty() {
                lines.push(cleaned);
            }
        }

        lines
    }

    /// Whatever is buffered but unterminated, for a final flush at exit.
    ///
    /// The engine's last words before a crash are often unterminated, and they
    /// are exactly the words worth having.
    pub fn flush(&mut self) -> Option<String> {
        if self.buffer.is_empty() {
            return None;
        }

        let raw = std::mem::take(&mut self.buffer);
        let text = String::from_utf8_lossy(&raw);
        let cleaned = clean(&text);
        (!cleaned.trim().is_empty()).then_some(cleaned)
    }

    /// Bytes held back waiting for a newline.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }
}

fn clean(text: &str) -> String {
    let stripped = strip_escapes(text);
    let trimmed = stripped.trim_end_matches(['\n', '\r']);
    collapse_carriage_returns(trimmed).to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_colour_the_engine_writes() {
        // GameLog.cs sets a foreground colour before each field.
        let raw = "\u{1b}[36m02:04:11 \u{1b}[32mIdentity\u{1b}[0m Kyle joined";
        assert_eq!(strip_escapes(raw), "02:04:11 Identity Kyle joined");
    }

    #[test]
    fn strips_cursor_positioning() {
        assert_eq!(strip_escapes("\u{1b}[2;1Hstatus"), "status");
        assert_eq!(strip_escapes("\u{1b}[Kcleared"), "cleared");
    }

    #[test]
    fn strips_an_osc_title_set() {
        assert_eq!(strip_escapes("\u{1b}]0;a title\u{7}text"), "text");
        assert_eq!(strip_escapes("\u{1b}]0;a title\u{1b}\\text"), "text");
    }

    #[test]
    fn keeps_only_the_last_redraw_of_a_line() {
        assert_eq!(collapse_carriage_returns("old\rnewer\rnewest"), "newest");
        assert_eq!(collapse_carriage_returns("untouched"), "untouched");
    }

    #[test]
    fn assembles_lines_across_arbitrary_read_boundaries() {
        let mut assembler = LineAssembler::new();
        assert!(assembler.push(b"02:04:11 Identity Ky").is_empty());
        assert!(assembler.push(b"le joined").is_empty());

        let lines = assembler.push(b"\n02:04:12 Chat     hi\n");
        assert_eq!(
            lines,
            vec!["02:04:11 Identity Kyle joined", "02:04:12 Chat     hi"]
        );
    }

    #[test]
    fn holds_an_escape_sequence_split_across_a_read() {
        let mut assembler = LineAssembler::new();
        assembler.push(b"\x1b[3");
        let lines = assembler.push(b"6mcoloured\n");
        assert_eq!(lines, vec!["coloured"]);
    }

    #[test]
    fn survives_a_multibyte_character_split_across_a_read() {
        let mut assembler = LineAssembler::new();
        let name = "Ky★le".as_bytes();
        assembler.push(&name[..3]);
        let lines = assembler.push(&name[3..]);
        assert!(lines.is_empty());
        assembler.push(b"\n");
    }

    #[test]
    fn flush_returns_an_unterminated_final_line() {
        let mut assembler = LineAssembler::new();
        assembler.push(b"fatal: the last thing it said");
        assert_eq!(
            assembler.flush().as_deref(),
            Some("fatal: the last thing it said")
        );
        assert!(assembler.flush().is_none());
    }

    #[test]
    fn blank_lines_are_not_emitted() {
        let mut assembler = LineAssembler::new();
        assert!(assembler.push(b"\n   \n\t\n").is_empty());
    }
}

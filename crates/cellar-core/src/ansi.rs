//! Making a pseudo-terminal stream readable.
//! The dedicated console is an interactive overlay: it colours log lines and it redraws a status bar in place using cursor movement and carriage returns. A supervisor reading the pty gets all of that mixed into the log text.
//! Rather than track cursor state (which needs a full terminal emulator to do correctly), this strips control sequences and then lets the caller identify the status bar by its *shape*, via `grammar::parse_status_bar`. That is both simpler and more robust: it survives the engine changing how it positions the bar, and it fails by treating a status line as a log line rather than by losing output.

/// Strip ANSI escape sequences from a chunk of terminal output.
/// Handles the two forms the engine's colouring produces, CSI (`ESC [ ... cmd`) and OSC (`ESC ] ... BEL` or `ESC ] ... ESC \`), plus two-character escapes.
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
/// A status bar redrawn with `\r` leaves several versions of itself in one physical line. Only the last one was ever visible, so that is the one kept.
pub fn collapse_carriage_returns(line: &str) -> &str {
    match line.rfind('\r') {
        Some(at) => &line[at + 1..],
        None => line,
    }
}

/// Accumulates pty bytes and yields whole, cleaned lines.
/// A pty read boundary lands anywhere, including inside a UTF-8 sequence and inside an escape sequence, so partial input is held rather than parsed.
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
            let cleaned = clean(&raw);
            if !cleaned.trim().is_empty() {
                lines.push(cleaned);
            }
        }

        lines
    }

    /// Whatever is buffered but unterminated, for a final flush at exit.
    /// The engine's last words before a crash are often unterminated, and they are exactly the words worth having.
    pub fn flush(&mut self) -> Option<String> {
        if self.buffer.is_empty() {
            return None;
        }

        let raw = std::mem::take(&mut self.buffer);
        let cleaned = clean(&raw);
        (!cleaned.trim().is_empty()).then_some(cleaned)
    }

    /// Bytes held back waiting for a newline.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }
}

fn clean(bytes: &[u8]) -> String {
    let stripped = strip_terminal_bytes(bytes);
    let trimmed = stripped.trim_end_matches(['\n', '\r']);
    normalize_headless_redraw(collapse_carriage_returns(trimmed))
}

fn normalize_headless_redraw(line: &str) -> String {
    let Some(at) = line.find("\u{fffd}") else {
        return line.to_owned();
    };

    let Some(timestamp) = (at..line.len()).find_map(|index| {
        let candidate = line.get(index..)?;
        let bytes = candidate.as_bytes();
        (bytes.len() >= 9
            && bytes[2] == b':'
            && bytes[5] == b':'
            && bytes[8] == b' '
            && bytes[..8]
                .iter()
                .enumerate()
                .all(|(offset, byte)| (offset == 2 || offset == 5) || byte.is_ascii_digit()))
        .then_some(index)
    }) else {
        return String::new();
    };

    line[timestamp..].to_owned()
}

fn strip_terminal_bytes(bytes: &[u8]) -> String {
    let mut clean = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == 0x1b {
            index += 1;
            if bytes.get(index) == Some(&b'[') {
                index += 1;
                while let Some(&candidate) = bytes.get(index) {
                    index += 1;
                    if (0x40..=0x7e).contains(&candidate) {
                        break;
                    }
                }
            } else if bytes.get(index) == Some(&b']') {
                index += 1;
                skip_string_control(bytes, &mut index);
            } else {
                index += usize::from(index < bytes.len());
            }
            continue;
        }
        if byte == 0x9b {
            index += 1;
            while let Some(&candidate) = bytes.get(index) {
                index += 1;
                if (0x40..=0x7e).contains(&candidate) {
                    break;
                }
            }
            continue;
        }
        if matches!(byte, 0x90 | 0x98 | 0x9d | 0x9e | 0x9f) {
            index += 1;
            skip_string_control(bytes, &mut index);
            continue;
        }
        if byte == 0x9c || (byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t')) {
            index += 1;
            continue;
        }
        clean.push(byte);
        index += 1;
    }
    String::from_utf8_lossy(&clean).into_owned()
}

fn skip_string_control(bytes: &[u8], index: &mut usize) {
    while let Some(&byte) = bytes.get(*index) {
        *index += 1;
        if byte == 0x07 || byte == 0x9c {
            break;
        }
        if byte == 0x1b && bytes.get(*index) == Some(&b'\\') {
            *index += 1;
            break;
        }
    }
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
    fn strips_c1_terminal_controls_before_utf8_decoding() {
        let raw = b"\x9d0;engine status\x07reply\n";
        let mut assembler = LineAssembler::new();
        assert_eq!(assembler.push(raw), vec!["reply"]);
    }

    #[test]
    fn drops_headless_cursor_redraw_artifacts() {
        let raw = "��������]E\x1b[6n                                                                            \n";
        let mut assembler = LineAssembler::new();
        assert!(assembler.push(raw.as_bytes()).is_empty());
    }

    #[test]
    fn preserves_a_console_log_after_a_headless_redraw_artifact() {
        let raw = "��������]E11:31:28 engine/R Reloaded 5 resident symlinked resources in 0ms\n";
        let mut assembler = LineAssembler::new();
        assert_eq!(
            assembler.push(raw.as_bytes()),
            vec!["11:31:28 engine/R Reloaded 5 resident symlinked resources in 0ms"]
        );
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

//! Following `logs/sbox-server.log`.
//!
//! The better of the two channels to parse. The console truncates every logger
//! name to eight characters and carries a 12-hour clock with no date; the file
//! is tab separated, dated to four decimal places, and keeps the logger name
//! whole.
//!
//! What it has to survive, all from the engine's own NLog configuration:
//!
//! - the file not existing yet, because the server has not started;
//! - `ArchiveOldFileOnStartup = true`, so every restart renames the file away
//!   and begins a new one;
//! - `ArchiveAboveSize = 512MB` and `ArchiveEvery = Day`, the same rename mid-run;
//! - `KeepFileOpen = true`, so the writer holds its handle open throughout.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Follows one file, tolerating it being rotated, truncated or absent.
pub struct Tailer {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    position: u64,
    partial: Vec<u8>,
    /// How many times the file was seen to rotate. Surfaced so an operator can
    /// tell "the log went quiet" from "the log moved".
    rotations: u64,
    /// Whether the follow position has been fixed yet.
    ///
    /// Only the first poll may skip to the end, and only if the file already
    /// existed. A file that appears later belongs entirely to this run, so it is
    /// read from its first byte: seeking to the end of a file the server has just
    /// created loses every line it wrote while starting up, which is most of the
    /// lines worth having.
    armed: bool,
}

impl Tailer {
    /// Follow `path`, starting at its end.
    ///
    /// From the end, not the beginning: a restart must not replay a 500 MB
    /// archive as if every historical player had just joined.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            reader: None,
            position: 0,
            partial: Vec::new(),
            rotations: 0,
            armed: false,
        }
    }

    /// Follow from the beginning instead, for reading a run that already happened.
    pub fn from_start(path: impl Into<PathBuf>) -> Self {
        let mut tailer = Self::new(path);
        tailer.position = 0;
        tailer.armed = true;
        tailer.open(false);
        tailer
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rotations(&self) -> u64 {
        self.rotations
    }

    /// Read whatever has been appended since the last call.
    ///
    /// Never blocks and never fails: a missing or unreadable file is "nothing new
    /// yet", because the common case for both is a server that has not written
    /// its first line.
    pub fn poll(&mut self) -> Vec<String> {
        if self.reader.is_none() {
            // Skip to the end only on the very first poll, and only if the file
            // was already there. A file created after arming is this run's own.
            let skip_history = !self.armed && self.path.exists();
            self.armed = true;
            self.open(skip_history);
        }

        if self.reader.is_none() {
            return Vec::new();
        }

        // A file shorter than where we are reading was rotated or truncated
        // underneath us. Start the new one from its beginning.
        if let Ok(metadata) = std::fs::metadata(&self.path)
            && metadata.len() < self.position
        {
            self.rotations += 1;
            self.reader = None;
            self.position = 0;
            self.partial.clear();
            self.open(false);
        }

        let Some(reader) = self.reader.as_mut() else {
            return Vec::new();
        };

        let mut buffer = Vec::new();
        if reader.read_to_end(&mut buffer).is_err() {
            self.reader = None;
            return Vec::new();
        }

        if buffer.is_empty() {
            return Vec::new();
        }

        self.position += buffer.len() as u64;
        self.partial.extend_from_slice(&buffer);
        self.take_complete_lines()
    }

    /// Whatever is buffered without a trailing newline.
    ///
    /// The engine's last line before a crash is often unterminated, and it is
    /// usually the one worth reading.
    pub fn flush(&mut self) -> Option<String> {
        if self.partial.is_empty() {
            return None;
        }

        let raw = std::mem::take(&mut self.partial);
        let text = String::from_utf8_lossy(&raw).trim_end().to_owned();
        (!text.is_empty()).then_some(text)
    }

    fn take_complete_lines(&mut self) -> Vec<String> {
        let mut lines = Vec::new();

        while let Some(at) = self.partial.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.partial.drain(..=at).collect();
            let text = String::from_utf8_lossy(&raw);
            let trimmed = text.trim_end_matches(['\r', '\n']);
            if !trimmed.trim().is_empty() {
                lines.push(trimmed.to_owned());
            }
        }

        lines
    }

    fn open(&mut self, seek_to_end: bool) {
        let Ok(file) = File::open(&self.path) else {
            return;
        };

        let mut reader = BufReader::new(file);

        if seek_to_end {
            match reader.seek(SeekFrom::End(0)) {
                Ok(at) => self.position = at,
                Err(_) => return,
            }
        } else if reader.seek(SeekFrom::Start(self.position)).is_err() {
            return;
        }

        self.reader = Some(reader);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::Write;

    use super::*;

    fn append(path: &Path, text: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
        file.flush().unwrap();
    }

    #[test]
    fn a_missing_file_is_quiet_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut tailer = Tailer::new(dir.path().join("not-yet.log"));
        assert!(tailer.poll().is_empty());
        assert!(tailer.poll().is_empty());
    }

    #[test]
    fn it_reads_only_what_arrives_after_it_starts_following() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sbox-server.log");
        append(&path, "old line that must not be replayed\n");

        let mut tailer = Tailer::new(&path);
        assert!(tailer.poll().is_empty(), "history is not replayed");

        append(&path, "a new line\n");
        assert_eq!(tailer.poll(), vec!["a new line"]);
    }

    #[test]
    fn from_start_reads_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sbox-server.log");
        append(&path, "first\nsecond\n");

        let mut tailer = Tailer::from_start(&path);
        assert_eq!(tailer.poll(), vec!["first", "second"]);
    }

    #[test]
    fn a_line_split_across_two_writes_is_held_until_it_is_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sbox-server.log");
        append(&path, "");

        let mut tailer = Tailer::new(&path);
        assert!(tailer.poll().is_empty(), "arm the follow position");

        append(&path, "2026/08/25 14:02:11.1234\t[Identity] Kyle ");
        assert!(tailer.poll().is_empty(), "a half line is not a line");

        append(&path, "is connected\n");
        assert_eq!(
            tailer.poll(),
            vec!["2026/08/25 14:02:11.1234\t[Identity] Kyle is connected"]
        );
    }

    /// `ArchiveOldFileOnStartup = true` means every restart does this.
    #[test]
    fn rotation_is_detected_and_the_new_file_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sbox-server.log");
        append(&path, "");

        let mut tailer = Tailer::new(&path);
        assert!(tailer.poll().is_empty(), "arm the follow position");

        append(&path, "before the restart\n");
        assert_eq!(tailer.poll(), vec!["before the restart"]);

        // The engine renames the file away and opens a fresh one.
        std::fs::rename(&path, dir.path().join("sbox-server-2026-08-25.log")).unwrap();
        append(&path, "after the restart\n");

        assert_eq!(tailer.poll(), vec!["after the restart"]);
        assert_eq!(tailer.rotations(), 1);
    }

    #[test]
    fn truncation_in_place_is_treated_as_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sbox-server.log");
        append(&path, "a long first line that sets the position\n");

        let mut tailer = Tailer::new(&path);
        assert!(tailer.poll().is_empty(), "arm the follow position");

        append(&path, "second\n");
        assert_eq!(tailer.poll(), vec!["second"]);

        std::fs::write(&path, "tiny\n").unwrap();
        assert_eq!(tailer.poll(), vec!["tiny"]);
        assert_eq!(tailer.rotations(), 1);
    }

    #[test]
    fn blank_lines_are_not_emitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sbox-server.log");
        append(&path, "");

        let mut tailer = Tailer::new(&path);
        append(&path, "\n\n   \n");
        assert!(tailer.poll().is_empty());
    }

    #[test]
    fn flush_yields_an_unterminated_final_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sbox-server.log");
        append(&path, "");

        let mut tailer = Tailer::new(&path);
        assert!(tailer.poll().is_empty(), "arm the follow position");

        append(&path, "fatal, and no newline followed");
        assert!(tailer.poll().is_empty());
        assert_eq!(
            tailer.flush().as_deref(),
            Some("fatal, and no newline followed")
        );
        assert!(tailer.flush().is_none());
    }
}

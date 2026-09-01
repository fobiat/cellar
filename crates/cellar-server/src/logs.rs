//! Persistent engine-log scanning for the operator console.
//!
//! The engine owns the log files. Cellar reads the current file and rotated
//! siblings on demand, so a restart does not erase the searchable history and
//! no dashboard data is written into the gamemode checkout.

use std::path::{Path, PathBuf};

use cellar_core::event::{Event, Level, Origin};
use cellar_core::grammar::Line;
use cellar_core::profile::{Category, GamemodeProfile};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: Option<String>,
    pub tag: Option<String>,
    /// Exactly this severity.
    pub level: Option<Level>,
    /// This severity or worse. The console's severity control is a threshold,
    /// so searching the rotated files has to be one too or the live view and
    /// the search disagree about the same control.
    pub level_min: Option<Level>,
    pub category: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct Record {
    pub at: DateTime<Utc>,
    pub level: Level,
    pub tag: String,
    pub category: Category,
    pub message: String,
    pub origin: Origin,
    pub raw: String,
    pub file: String,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub lines: Vec<Record>,
    pub matched: usize,
    pub scanned_files: usize,
    pub scanned_lines: usize,
    pub generated_at: DateTime<Utc>,
    pub persistent: bool,
}

/// Scan the engine's log files.
///
/// Takes the profile rather than reading a global one: the categories a line
/// falls into depend on the gamemode, and with two instances in a process there
/// is no single right answer to fetch from somewhere else.
pub async fn search(path: &Path, profile: &GamemodeProfile, query: &Query) -> SearchResult {
    let files = files_for(path).await;
    let mut lines = Vec::new();
    let mut matched = 0;
    let mut scanned_lines = 0;

    for file in &files {
        let Ok(handle) = tokio::fs::File::open(file).await else {
            continue;
        };
        let mut reader = BufReader::new(handle).lines();
        while let Ok(Some(raw)) = reader.next_line().await {
            scanned_lines += 1;
            let Some(record) = record(file, profile, &raw) else {
                continue;
            };
            if !matches(query, &record) {
                continue;
            }
            matched += 1;
            lines.push(record);
            if lines.len() > query.limit {
                lines.remove(0);
            }
        }
    }

    SearchResult {
        lines,
        matched,
        scanned_files: files.len(),
        scanned_lines,
        generated_at: Utc::now(),
        persistent: true,
    }
}

async fn files_for(current: &Path) -> Vec<PathBuf> {
    let Some(parent) = current.parent() else {
        return vec![current.to_owned()];
    };
    let mut files = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return vec![current.to_owned()];
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let candidate = entry.path();
        let is_log = candidate
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("sbox-server") && name.ends_with(".log"));
        if is_log {
            files.push(candidate);
        }
    }
    if files.is_empty() {
        files.push(current.to_owned());
    }
    files.sort();
    files
}

fn record(file: &Path, profile: &GamemodeProfile, raw: &str) -> Option<Record> {
    let parsed = cellar_core::grammar::parse_line(Line::log_file(raw))?;
    // An empty ready pattern on purpose: a historical readiness line in a
    // rotated file is not this process becoming ready, and classifying it as
    // one would drop the line from the search rather than list it.
    let event = cellar_core::grammar::classify(&parsed, Origin::LogFile, "", profile);
    let Event::Log(line) = event else {
        return None;
    };
    Some(Record {
        at: line.at,
        level: line.level,
        category: line.category,
        tag: line.logger,
        message: line.message,
        origin: line.origin,
        raw: raw.to_owned(),
        file: file.to_string_lossy().into_owned(),
    })
}

fn matches(query: &Query, record: &Record) -> bool {
    if let Some(level) = query.level
        && level != record.level
    {
        return false;
    }
    if let Some(floor) = query.level_min
        && record.level < floor
    {
        return false;
    }
    if let Some(tag) = &query.tag
        && !record.tag.eq_ignore_ascii_case(tag)
    {
        return false;
    }
    if let Some(category) = &query.category
        && !record.category.as_str().eq_ignore_ascii_case(category)
    {
        return false;
    }
    query.text.as_deref().is_none_or(|text| {
        let text = text.to_ascii_lowercase();
        record.message.to_ascii_lowercase().contains(&text)
            || record.tag.to_ascii_lowercase().contains(&text)
            || record.raw.to_ascii_lowercase().contains(&text)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn record_at(level: Level) -> Record {
        Record {
            at: Utc::now(),
            level,
            tag: "Bootstrap".to_owned(),
            category: Category::Network,
            message: "Lobby created".to_owned(),
            origin: Origin::LogFile,
            raw: "Lobby created".to_owned(),
            file: "sbox-server.log".to_owned(),
        }
    }

    #[test]
    fn level_min_admits_everything_at_or_above_the_floor() {
        let query = Query {
            level_min: Some(Level::Warning),
            ..Default::default()
        };
        assert!(!matches(&query, &record_at(Level::Info)));
        assert!(matches(&query, &record_at(Level::Warning)));
        assert!(matches(&query, &record_at(Level::Error)));
    }

    #[test]
    fn level_stays_an_exact_match_for_callers_that_want_one() {
        let query = Query {
            level: Some(Level::Warning),
            ..Default::default()
        };
        assert!(!matches(&query, &record_at(Level::Error)));
        assert!(matches(&query, &record_at(Level::Warning)));
    }
}

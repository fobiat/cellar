//! Persistent engine-log scanning for the operator console.
//!
//! The engine owns the log files. Cellar reads the current file and rotated
//! siblings on demand, so a restart does not erase the searchable history and
//! no dashboard data is written into the gamemode checkout.

use std::path::{Path, PathBuf};

use cellar_core::event::{Event, Level, Origin};
use cellar_core::grammar::Line;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: Option<String>,
    pub tag: Option<String>,
    pub level: Option<Level>,
    pub category: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct Record {
    pub at: DateTime<Utc>,
    pub level: Level,
    pub tag: String,
    pub category: String,
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

pub async fn search(path: &Path, query: &Query) -> SearchResult {
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
            let Some(record) = record(file, &raw) else {
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

fn record(file: &Path, raw: &str) -> Option<Record> {
    let parsed = cellar_core::grammar::parse_line(Line::log_file(raw))?;
    let event = cellar_core::grammar::classify(&parsed, Origin::LogFile, "");
    let Event::Log(line) = event else {
        return None;
    };
    let category = category(&line.logger, &line.message);
    Some(Record {
        at: line.at,
        level: line.level,
        tag: line.logger,
        category,
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
    if let Some(tag) = &query.tag
        && !record.tag.eq_ignore_ascii_case(tag)
    {
        return false;
    }
    if let Some(category) = &query.category
        && !record.category.eq_ignore_ascii_case(category)
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

pub fn category(tag: &str, message: &str) -> String {
    let text = format!("{} {}", tag, message).to_ascii_lowercase();
    if text.contains("storage") || text.contains("database") || text.contains("document") {
        "storage".to_owned()
    } else if text.contains("network") || text.contains("connect") || text.contains("lobby") {
        "network".to_owned()
    } else if text.contains("player") || text.contains("identity") || text.contains("chat") {
        "players".to_owned()
    } else if text.contains("physics") || text.contains("render") || text.contains("map") {
        "engine".to_owned()
    } else if text.contains("applejack") || text.contains("game") {
        "gameplay".to_owned()
    } else if text.contains("cellar") {
        "cellar".to_owned()
    } else {
        "other".to_owned()
    }
}

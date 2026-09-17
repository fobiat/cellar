//! Verified, portable snapshots of the bridge document store.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::state::SnapshotDocument;

const FORMAT: u32 = 1;
const PREFIX: &str = "cellar-persistence-";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: u32,
    pub scope: String,
    pub created_at: DateTime<Utc>,
    pub documents: Vec<SnapshotDocument>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: DateTime<Utc>,
    pub verified: bool,
    pub documents: Option<usize>,
    pub error: Option<String>,
}

pub fn list(directory: &Path) -> Vec<Entry> {
    let Ok(read_dir) = std::fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut entries = read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            if !name.starts_with(PREFIX) || path.extension().is_none_or(|ext| ext != "json") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().ok().map(DateTime::<Utc>::from)?;
            let checked = read(&path);
            Some(Entry {
                name,
                path,
                bytes: metadata.len(),
                modified,
                verified: checked.is_ok(),
                documents: checked
                    .as_ref()
                    .ok()
                    .map(|snapshot| snapshot.documents.len()),
                error: checked.err().map(|why| why.to_string()),
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by_key(|entry| std::cmp::Reverse(entry.modified));
    entries
}

pub fn create(
    directory: &Path,
    copy_to: Option<&Path>,
    retain: usize,
    verify: bool,
    scope: &str,
    documents: Vec<SnapshotDocument>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory)
        .map_err(|why| format!("could not create {}: {why}", directory.display()))?;

    let snapshot = Snapshot {
        format: FORMAT,
        scope: scope.to_owned(),
        created_at: Utc::now(),
        documents,
    };
    let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|why| why.to_string())?;
    let base_name = format!(
        "{PREFIX}{}.json",
        snapshot.created_at.format("%Y%m%dT%H%M%SZ")
    );
    let mut path = directory.join(&base_name);
    let mut suffix = 1;
    while path.exists() {
        path = directory.join(format!(
            "{}-{suffix}.json",
            base_name.trim_end_matches(".json")
        ));
        suffix += 1;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, &bytes)
        .map_err(|why| format!("could not write {}: {why}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .map_err(|why| format!("could not publish {}: {why}", path.display()))?;

    if verify {
        verify_snapshot(&path, scope)?;
    }

    if let Some(copy_to) = copy_to {
        std::fs::create_dir_all(copy_to)
            .map_err(|why| format!("could not create {}: {why}", copy_to.display()))?;
        let copy = copy_to.join(path.file_name().ok_or("snapshot has no file name")?);
        std::fs::copy(&path, &copy)
            .map_err(|why| format!("could not export {}: {why}", copy.display()))?;
        if verify {
            verify_snapshot(&copy, scope)?;
        }
    }

    prune(directory, retain)?;
    Ok(path)
}

pub fn read(path: &Path) -> Result<Snapshot, String> {
    let bytes =
        std::fs::read(path).map_err(|why| format!("could not read {}: {why}", path.display()))?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes)
        .map_err(|why| format!("invalid persistence snapshot {}: {why}", path.display()))?;
    if snapshot.format != FORMAT {
        return Err(format!(
            "unsupported persistence snapshot format {}",
            snapshot.format
        ));
    }
    let mut keys = std::collections::HashSet::with_capacity(snapshot.documents.len());
    for document in &snapshot.documents {
        cellar_core::doc_key::check(&document.key).map_err(|why| {
            format!(
                "persistence snapshot contains invalid key '{}': {why}",
                document.key
            )
        })?;
        if !keys.insert(&document.key) {
            return Err(format!(
                "persistence snapshot contains duplicate key '{}'",
                document.key
            ));
        }
    }
    Ok(snapshot)
}

pub fn verify_snapshot(path: &Path, scope: &str) -> Result<Snapshot, String> {
    let snapshot = read(path)?;
    if snapshot.scope != scope {
        return Err(format!(
            "snapshot belongs to scope '{}', not '{scope}'",
            snapshot.scope
        ));
    }
    Ok(snapshot)
}

fn prune(directory: &Path, retain: usize) -> Result<(), String> {
    for entry in list(directory).into_iter().skip(retain) {
        std::fs::remove_file(&entry.path)
            .map_err(|why| format!("could not prune {}: {why}", entry.path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cellar-persistence-{name}-{}", std::process::id()))
    }

    #[test]
    fn creates_verifies_exports_and_prunes_snapshots() {
        let source = test_directory("source");
        let export = test_directory("export");
        std::fs::remove_dir_all(&source).ok();
        std::fs::remove_dir_all(&export).ok();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&export).unwrap();
        let documents = vec![SnapshotDocument {
            key: "characters/one.json".to_owned(),
            body: serde_json::json!({"name": "one"}),
        }];

        let path = create(&source, Some(&export), 1, true, "example", documents).unwrap();
        assert_eq!(list(&source).len(), 1);
        assert_eq!(list(&export).len(), 1);
        assert_eq!(
            verify_snapshot(&path, "example").unwrap().documents.len(),
            1
        );
        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(export).unwrap();
    }

    #[test]
    fn rejects_a_snapshot_from_another_scope() {
        let source = test_directory("scope");
        std::fs::remove_dir_all(&source).ok();
        std::fs::create_dir_all(&source).unwrap();
        let path = create(&source, None, 7, true, "one", Vec::new()).unwrap();
        assert!(verify_snapshot(&path, "two").is_err());
        std::fs::remove_dir_all(source).unwrap();
    }

    #[test]
    fn rejects_invalid_or_duplicate_document_keys_before_restore() {
        let source = test_directory("invalid-keys");
        std::fs::remove_dir_all(&source).ok();
        std::fs::create_dir_all(&source).unwrap();
        let path = source.join("snapshot.json");
        let snapshot = Snapshot {
            format: FORMAT,
            scope: "one".to_owned(),
            created_at: Utc::now(),
            documents: vec![
                SnapshotDocument {
                    key: "bad key.json".to_owned(),
                    body: serde_json::json!({}),
                },
                SnapshotDocument {
                    key: "bad key.json".to_owned(),
                    body: serde_json::json!({}),
                },
            ],
        };
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

        assert!(verify_snapshot(&path, "one").is_err());
        std::fs::remove_dir_all(source).unwrap();
    }
}

//! Safe profile discovery and switching for a running Cellar instance.

use std::path::{Path, PathBuf};

use cellar_core::config::Config;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Profile {
    pub name: String,
    pub mode: &'static str,
    pub path: String,
    pub active: bool,
    pub game: Option<String>,
    pub project: String,
    pub map: Option<String>,
}

pub async fn list(directory: &Path, active: Option<&Path>) -> Vec<Profile> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return Vec::new();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| profile(path, active))
        .collect()
}

pub fn resolve(directory: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains(char::from(92))
        || name.contains(':')
        || name != name.trim()
    {
        return Err("profile names must be a simple TOML filename stem".to_owned());
    }
    let path = directory.join(format!("{name}.toml"));
    if !path.is_file() {
        return Err(format!("profile '{name}' does not exist"));
    }
    Ok(path)
}

pub fn load(path: &Path) -> Result<Config, String> {
    Config::load(path).map_err(|error| format!("could not load {}: {error}", path.display()))
}

fn profile(path: PathBuf, active: Option<&Path>) -> Option<Profile> {
    let config = Config::load(&path).ok()?;
    let server = config.primary_server()?;
    Some(Profile {
        name: path.file_stem()?.to_string_lossy().into_owned(),
        mode: if server.is_published() {
            "published"
        } else {
            "development"
        },
        path: path.to_string_lossy().into_owned(),
        active: active.is_some_and(|current| current == path.as_path()),
        game: server.game,
        project: server.project.to_string_lossy().into_owned(),
        map: server.map,
    })
}

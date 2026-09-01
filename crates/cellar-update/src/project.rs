//! Reading a `.sbproj`, which is where the facts Cellar cannot pass live.
//!
//! Two of them matter operationally.
//!
//! **The player ceiling is here and nowhere else.** `+maxplayers` is not a
//! convar and not a launch switch; the old `entrypoint.sh` passed it for years
//! and it was inert. `Metadata.MaxPlayers` in this file is the real number, and
//! until now nothing in Cellar read it, so the dashboard showed a ceiling of
//! zero until the engine's status bar happened to mention one.
//!
//! **The package ident is derived from `Org` and `Ident`**, which is what the
//! engine names the data directory after and what `+game` takes. Getting it
//! wrong writes `hosting.json` where the game never reads it.
//!
//! Parsed defensively and reported as what was found. The schema is not
//! documented field by field anywhere Cellar can cite, so a missing key is an
//! absence rather than an error, and nothing here fails a server that starts.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// What a `.sbproj` says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub title: Option<String>,
    pub org: Option<String>,
    pub ident: Option<String>,
    /// `Metadata.MaxPlayers`. The real ceiling, and the only place it exists.
    pub max_players: Option<u32>,
    /// `Metadata.MapList`, when the project declares one. Best effort: the
    /// field is not in any document Cellar can cite, so an absent list means
    /// "this project did not say", never "this project has no maps".
    pub maps: Vec<String>,
    /// `Metadata.PackageReferences`, same caveat. Useful because the engine
    /// resolves these from sbox.game at boot and a resolution failure is one of
    /// the observed ways a server starts and never becomes ready.
    pub packages: Vec<String>,
}

impl Project {
    /// `org.ident`, which is what `+game` takes for a published package.
    ///
    /// The engine appends `#local` to this for a local `.sbproj`, which is why
    /// development and published modes read different data directories.
    pub fn package_ident(&self) -> Option<String> {
        let (org, ident) = (self.org.as_ref()?, self.ident.as_ref()?);
        Some(format!("{org}.{ident}"))
    }
}

/// Read and parse a `.sbproj`.
///
/// `Ok(None)` when the path is not a project file or does not exist, which is
/// the normal case for an instance running a published package: there is no
/// source tree on that box at all.
pub fn read(path: &Path) -> Result<Option<Project>, String> {
    if path.as_os_str().is_empty() || !path.is_file() {
        return Ok(None);
    }

    let text = std::fs::read_to_string(path)
        .map_err(|why| format!("reading {}: {why}", path.display()))?;
    parse(&text).map(Some)
}

/// The parsing half, separated so it is testable without a file.
pub fn parse(text: &str) -> Result<Project, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|why| format!("not JSON: {why}"))?;

    let string = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|found| !found.trim().is_empty())
            .map(str::to_owned)
    };

    let metadata = value.get("Metadata");
    let strings = |key: &str| {
        metadata
            .and_then(|meta| meta.get(key))
            .and_then(serde_json::Value::as_array)
            .map(|found| {
                found
                    .iter()
                    .filter_map(|entry| match entry {
                        // A package reference is sometimes a bare ident and
                        // sometimes an object carrying one. Take either rather
                        // than deciding which the schema "really" is.
                        serde_json::Value::String(ident) => Some(ident.clone()),
                        serde_json::Value::Object(_) => entry
                            .get("Ident")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        _ => None,
                    })
                    .filter(|ident| !ident.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };

    Ok(Project {
        title: string("Title"),
        org: string("Org"),
        ident: string("Ident"),
        max_players: metadata
            .and_then(|meta| meta.get("MaxPlayers"))
            .and_then(serde_json::Value::as_u64)
            // A ceiling that does not fit in a u32 is a typo, not a ceiling.
            .and_then(|found| u32::try_from(found).ok())
            .filter(|found| *found > 0),
        maps: strings("MapList"),
        packages: strings("PackageReferences"),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_player_ceiling_comes_off_the_project_and_not_off_a_switch() {
        let project = parse(
            r#"{
              "Title": "AppleJackRP",
              "Org": "fobiat",
              "Ident": "applejackrp",
              "Metadata": { "MaxPlayers": 32 }
            }"#,
        )
        .unwrap();

        assert_eq!(project.max_players, Some(32));
        assert_eq!(
            project.package_ident().as_deref(),
            Some("fobiat.applejackrp")
        );
        assert_eq!(project.title.as_deref(), Some("AppleJackRP"));
    }

    #[test]
    fn a_project_that_says_nothing_is_an_absence_rather_than_an_error() {
        // The schema is not documented key by key anywhere Cellar can cite, so
        // a project missing every field it might have read has to parse.
        let project = parse(r#"{ "Schema": 1 }"#).unwrap();

        assert_eq!(project.max_players, None);
        assert_eq!(project.package_ident(), None);
        assert!(project.maps.is_empty());
        assert!(project.packages.is_empty());
    }

    #[test]
    fn a_package_reference_is_taken_as_a_string_or_as_an_object() {
        let project = parse(
            r#"{
              "Metadata": {
                "MapList": ["facepunch.flatgrass", "  "],
                "PackageReferences": [
                  "facepunch.sbox_content",
                  { "Ident": "fobiat.somelibrary" },
                  { "NotAnIdent": "ignored" },
                  17
                ]
              }
            }"#,
        )
        .unwrap();

        assert_eq!(project.maps, ["facepunch.flatgrass"]);
        assert_eq!(
            project.packages,
            ["facepunch.sbox_content", "fobiat.somelibrary"]
        );
    }

    #[test]
    fn a_zero_ceiling_is_not_a_ceiling() {
        // `"MaxPlayers": 0` is what an unfilled template holds, and reporting
        // a server as 0/0 is worse than reporting it as unknown.
        let project = parse(r#"{ "Metadata": { "MaxPlayers": 0 } }"#).unwrap();
        assert_eq!(project.max_players, None);
    }

    #[test]
    fn something_that_is_not_json_says_so() {
        assert!(parse("<Project Sdk=\"Microsoft.NET.Sdk\">").is_err());
    }

    #[test]
    fn a_path_that_is_not_there_is_not_a_failure() {
        // Every published-package instance is this case: no source tree at all.
        assert_eq!(read(Path::new("/nonexistent/x.sbproj")), Ok(None));
    }
}

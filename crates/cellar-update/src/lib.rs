//! Knowing what is installed, what is available, and what to do about it.
//!
//! Three separable things: [`version`] answers "what is running", [`changelog`]
//! answers "what would change", and [`updater`] decides whether now is the time.
//! Keeping the decision pure is what makes "never restart a server with players
//! on it" a test rather than a hope.

pub mod changelog;
pub mod pipeline;
pub mod project;
pub mod selfupdate;
pub mod updater;
pub mod version;

pub use changelog::{Release, Section};
pub use project::Project;
pub use updater::{Decision, Policy, UpdateConfig};
pub use version::{Probe, Versions};

/// Everything the Versions tab shows, in one value.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub versions: Versions,
    pub decision: Decision,
    /// Newest first, capped: a changelog can be tens of thousands of words and
    /// the question being answered is "what changed recently".
    pub releases: Vec<Release>,
}

/// Read the changelog beside a project, if there is one.
pub fn read_changelog(project_dir: &std::path::Path, limit: usize) -> Vec<Release> {
    let path = project_dir.join("CHANGELOG.md");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };

    let mut releases = changelog::parse(&String::from_utf8_lossy(&bytes));
    releases.truncate(limit);
    releases
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_project_without_a_changelog_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_changelog(dir.path(), 5).is_empty());
    }

    #[test]
    fn the_release_list_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut markdown = String::from("# Changelog\n\n");
        for major in (1..=10).rev() {
            markdown.push_str(&format!(
                "## [{major}.0.0] - 2026-01-01\n\n### Added\n- a thing\n\n"
            ));
        }
        std::fs::write(dir.path().join("CHANGELOG.md"), markdown).unwrap();

        assert_eq!(read_changelog(dir.path(), 3).len(), 3);
    }
}

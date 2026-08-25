//! Reading `CHANGELOG.md`.
//!
//! AppleJackRP keeps a Keep a Changelog document and generates its in-game
//! version stamp from the top heading, so the changelog is not decoration: it is
//! the thing that decides what the build calls itself. Cellar parses the same
//! file so an operator can see what a pending update actually contains before
//! taking it.
//!
//! Strict about structure, forgiving about prose. A heading it does not
//! recognise ends the current release rather than being folded into it, because
//! attributing one release's entries to another is worse than showing fewer.

use serde::{Deserialize, Serialize};

/// One `### Added` style group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub items: Vec<String>,
}

/// One `## [1.2.0] - 2026-08-01` release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The version as written, with the brackets removed.
    pub version: String,
    pub date: Option<String>,
    pub sections: Vec<Section>,
}

impl Release {
    /// Whether this is the unreleased section at the top.
    pub fn is_unreleased(&self) -> bool {
        self.version.eq_ignore_ascii_case("unreleased")
    }

    /// Every item across every section, for a summary line.
    pub fn item_count(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// The first sentence of each item, for a notification that must be short.
    pub fn headlines(&self, limit: usize) -> Vec<String> {
        self.sections
            .iter()
            .flat_map(|section| section.items.iter())
            .take(limit)
            .map(|item| headline(item))
            .collect()
    }
}

/// Parse a changelog into releases, newest first.
pub fn parse(markdown: &str) -> Vec<Release> {
    let mut releases: Vec<Release> = Vec::new();
    let mut section: Option<Section> = None;
    let mut item: Option<String> = None;

    for line in markdown.lines() {
        let trimmed = line.trim_end();

        if let Some(heading) = trimmed.strip_prefix("## ") {
            finish_item(&mut item, &mut section);
            finish_section(&mut section, &mut releases);

            let (version, date) = split_heading(heading);
            releases.push(Release {
                version,
                date,
                sections: Vec::new(),
            });
            continue;
        }

        if let Some(heading) = trimmed.strip_prefix("### ") {
            finish_item(&mut item, &mut section);
            finish_section(&mut section, &mut releases);

            if releases.is_empty() {
                continue;
            }

            section = Some(Section {
                name: heading.trim().to_owned(),
                items: Vec::new(),
            });
            continue;
        }

        // A single-level heading ends everything: it is the document title, or a
        // structure this parser does not know.
        if trimmed.starts_with("# ") {
            finish_item(&mut item, &mut section);
            finish_section(&mut section, &mut releases);
            continue;
        }

        if section.is_none() {
            continue;
        }

        if let Some(bullet) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            finish_item(&mut item, &mut section);
            item = Some(bullet.trim().to_owned());
            continue;
        }

        // An indented, non-empty line continues the bullet above it. Applejack's
        // entries routinely run to eight wrapped lines, and treating each as its
        // own item would turn one change into eight.
        if let Some(current) = item.as_mut()
            && line.starts_with(char::is_whitespace)
            && !trimmed.trim().is_empty()
        {
            current.push(' ');
            current.push_str(trimmed.trim());
            continue;
        }

        if trimmed.trim().is_empty() {
            finish_item(&mut item, &mut section);
        }
    }

    finish_item(&mut item, &mut section);
    finish_section(&mut section, &mut releases);

    releases
}

fn finish_item(item: &mut Option<String>, section: &mut Option<Section>) {
    if let (Some(text), Some(section)) = (item.take(), section.as_mut())
        && !text.trim().is_empty()
    {
        section.items.push(text);
    }
}

fn finish_section(section: &mut Option<Section>, releases: &mut [Release]) {
    if let (Some(section), Some(release)) = (section.take(), releases.last_mut())
        && !section.items.is_empty()
    {
        release.sections.push(section);
    }
}

/// `[1.2.0] - 2026-08-01` into its parts.
fn split_heading(heading: &str) -> (String, Option<String>) {
    let heading = heading.trim();

    let (version, rest) = match heading.strip_prefix('[') {
        Some(after) => match after.split_once(']') {
            Some((version, rest)) => (version.trim().to_owned(), rest),
            None => (after.trim().to_owned(), ""),
        },
        None => match heading.split_once(" - ") {
            Some((version, rest)) => (version.trim().to_owned(), rest),
            None => (heading.to_owned(), ""),
        },
    };

    let date = rest.trim().trim_start_matches('-').trim().to_owned();

    (version, (!date.is_empty()).then_some(date))
}

/// The first sentence of an entry, with its markdown emphasis removed.
pub fn headline(item: &str) -> String {
    let plain = item.replace("**", "").replace('`', "");

    let end = plain
        .char_indices()
        .find(|(index, c)| *c == '.' && plain[index + 1..].starts_with(' '))
        .map(|(index, _)| index + 1)
        .unwrap_or(plain.len());

    let mut line = plain[..end].trim().to_owned();
    if line.chars().count() > 160 {
        line = line.chars().take(157).collect::<String>();
        line.push('…');
    }
    line
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Changelog

All notable changes are documented here.

## [Unreleased]

### Added
- **A big thing.** With a body that wraps
  onto a second line and a third.
- A smaller thing.

### Fixed
- A fix.

## [1.1.0] - 2026-08-01

### Added
- The first release.
";

    #[test]
    fn it_reads_releases_newest_first() {
        let releases = parse(SAMPLE);
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].version, "Unreleased");
        assert_eq!(releases[1].version, "1.1.0");
        assert_eq!(releases[1].date.as_deref(), Some("2026-08-01"));
    }

    #[test]
    fn the_unreleased_section_is_recognised() {
        let releases = parse(SAMPLE);
        assert!(releases[0].is_unreleased());
        assert!(!releases[1].is_unreleased());
    }

    /// Applejack's entries wrap over many lines. Treating each wrapped line as a
    /// separate item would turn one change into eight.
    #[test]
    fn a_wrapped_entry_stays_one_item() {
        let releases = parse(SAMPLE);
        let added = &releases[0].sections[0];

        assert_eq!(added.name, "Added");
        assert_eq!(added.items.len(), 2);
        assert!(added.items[0].contains("onto a second line and a third"));
        assert!(!added.items[0].contains('\n'));
    }

    #[test]
    fn sections_are_kept_apart() {
        let releases = parse(SAMPLE);
        let names: Vec<&str> = releases[0]
            .sections
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["Added", "Fixed"]);
        assert_eq!(releases[0].item_count(), 3);
    }

    #[test]
    fn a_headline_is_the_first_sentence_without_markdown() {
        assert_eq!(
            headline("**A big thing.** With a body that follows."),
            "A big thing."
        );
        assert_eq!(headline("No full stop here"), "No full stop here");
        assert_eq!(
            headline("Uses `code` in the title."),
            "Uses code in the title."
        );
    }

    #[test]
    fn a_very_long_headline_is_cut() {
        let long = format!("{}. and more", "x".repeat(400));
        let headline = headline(&long);
        assert!(headline.chars().count() <= 160);
        assert!(headline.ends_with('…'));
    }

    #[test]
    fn an_empty_or_headingless_document_yields_nothing_rather_than_panicking() {
        assert!(parse("").is_empty());
        assert!(parse("just prose, no headings").is_empty());
        assert!(parse("# Changelog\n\nnothing else").is_empty());
    }

    #[test]
    fn a_release_with_no_items_is_kept_but_empty() {
        let releases =
            parse("## [2.0.0] - 2026-01-01\n\n## [1.0.0] - 2025-01-01\n\n### Added\n- x\n");
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].item_count(), 0);
        assert_eq!(releases[1].item_count(), 1);
    }

    #[test]
    fn a_heading_without_brackets_still_parses() {
        let releases = parse("## 1.0.0 - 2026-05-05\n\n### Added\n- x\n");
        assert_eq!(releases[0].version, "1.0.0");
        assert_eq!(releases[0].date.as_deref(), Some("2026-05-05"));
    }

    #[test]
    fn headlines_are_capped_at_the_limit_asked_for() {
        let releases = parse(SAMPLE);
        assert_eq!(releases[0].headlines(2).len(), 2);
        assert_eq!(releases[0].headlines(99).len(), 3);
    }

    /// The real document, if it is on this machine. Not a fixture: the point is
    /// to notice when the upstream format changes.
    #[test]
    fn the_real_applejack_changelog_parses_if_present() {
        let path = std::path::Path::new("/home/kyle/Projects/AppleJackRP-sandbox/CHANGELOG.md");
        let Ok(markdown) = std::fs::read_to_string(path) else {
            eprintln!("skipping: {} is not on this machine", path.display());
            return;
        };

        let releases = parse(&markdown);
        assert!(
            !releases.is_empty(),
            "the real changelog produced no releases"
        );
        assert!(releases[0].is_unreleased());
        assert!(releases[0].item_count() > 0);
    }
}

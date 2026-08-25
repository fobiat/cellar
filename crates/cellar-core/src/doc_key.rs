//! What a document key may be.
//!
//! A direct port of AppleJackRP's `Code/Storage/DocumentKeys.cs`, which is the
//! authority. Both halves of the bridge have to agree on this exactly: the
//! gamemode refuses an illegal key before it ever reaches the wire, so a key
//! this module accepts but the C# would not is a key that can never arrive, and
//! one this module rejects but the C# would send is an outage.
//!
//! Nothing here sanitises. A key is accepted as given or refused by name.

/// Path separator between key segments.
pub const SEGMENT_SEPARATOR: char = '/';

/// Every key the gamemode composes carries this suffix.
pub const EXTENSION: &str = ".json";

/// Also the width of the bridge's `doc_key` column.
pub const MAXIMUM_LENGTH: usize = 128;

/// Why a key was refused, so the bridge can say which rule it broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRefusal {
    Empty,
    TooLong { length: usize },
    LeadingSeparator,
    TrailingSeparator,
    EmptySegment,
    IllegalCharacter { character: char },
    RelativeSegment,
    ReservedDeviceName { stem: String },
}

impl std::fmt::Display for KeyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "a key may not be empty"),
            Self::TooLong { length } => {
                write!(
                    f,
                    "a key is at most {MAXIMUM_LENGTH} characters, this one is {length}"
                )
            }
            Self::LeadingSeparator => write!(f, "a key may not begin with '{SEGMENT_SEPARATOR}'"),
            Self::TrailingSeparator => write!(f, "a key may not end with '{SEGMENT_SEPARATOR}'"),
            Self::EmptySegment => write!(f, "a key may not contain an empty segment"),
            Self::IllegalCharacter { character } => write!(
                f,
                "'{character}' is not legal in a key; only a-z, 0-9, '.', '-', '_' and '{SEGMENT_SEPARATOR}' are"
            ),
            Self::RelativeSegment => write!(f, "'.' and '..' are not legal segments"),
            Self::ReservedDeviceName { stem } => {
                write!(
                    f,
                    "'{stem}' is a reserved device name and would store nothing"
                )
            }
        }
    }
}

/// Accept a key, or say which rule refused it.
pub fn check(key: &str) -> Result<(), KeyRefusal> {
    if key.is_empty() {
        return Err(KeyRefusal::Empty);
    }

    // Counting chars, not bytes, to match the C# `key.Length` over UTF-16. Only
    // ASCII survives `is_legal_character` anyway, so the two agree in practice;
    // this keeps them agreeing on the refusal message for a rejected key too.
    let length = key.chars().count();
    if length > MAXIMUM_LENGTH {
        return Err(KeyRefusal::TooLong { length });
    }

    if key.starts_with(SEGMENT_SEPARATOR) {
        return Err(KeyRefusal::LeadingSeparator);
    }

    if key.ends_with(SEGMENT_SEPARATOR) {
        return Err(KeyRefusal::TrailingSeparator);
    }

    for segment in key.split(SEGMENT_SEPARATOR) {
        check_segment(segment)?;
    }

    Ok(())
}

/// Whether a key is legal, when the reason does not matter.
pub fn is_legal(key: &str) -> bool {
    check(key).is_ok()
}

fn check_segment(segment: &str) -> Result<(), KeyRefusal> {
    if segment.is_empty() {
        return Err(KeyRefusal::EmptySegment);
    }

    if let Some(character) = segment.chars().find(|c| !is_legal_character(*c)) {
        return Err(KeyRefusal::IllegalCharacter { character });
    }

    if segment == "." || segment == ".." {
        return Err(KeyRefusal::RelativeSegment);
    }

    let stem = stem_of(segment);
    if is_reserved_device_name(stem) {
        return Err(KeyRefusal::ReservedDeviceName {
            stem: stem.to_owned(),
        });
    }

    Ok(())
}

// Uppercase is refused rather than folded: two filesystems could disagree about
// two keys differing only in case.
fn is_legal_character(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_'
}

fn stem_of(segment: &str) -> &str {
    match segment.find('.') {
        Some(dot) => &segment[..dot],
        None => segment,
    }
}

// Lowercase only, because `is_legal_character` already refused every uppercase
// key. Opening "nul.json" on Windows stores nothing, silently.
fn is_reserved_device_name(stem: &str) -> bool {
    if matches!(stem, "con" | "prn" | "aux" | "nul") {
        return true;
    }

    let bytes = stem.as_bytes();
    if bytes.len() != 4 {
        return false;
    }

    let port = matches!(&bytes[..3], b"com" | b"lpt");
    port && bytes[3].is_ascii_digit() && bytes[3] != b'0'
}

/// The directory part of a key, or empty when it has none.
pub fn directory_of(key: &str) -> &str {
    match key.rfind(SEGMENT_SEPARATOR) {
        Some(0) | None => "",
        Some(separator) => &key[..separator],
    }
}

/// Compose `<directory>/<name>.json`, refusing anything the rules would.
pub fn compose(directory: &str, name: &str) -> Result<String, KeyRefusal> {
    if directory.is_empty() || name.is_empty() {
        return Err(KeyRefusal::Empty);
    }

    let key = format!("{directory}{SEGMENT_SEPARATOR}{name}{EXTENSION}");
    check(&key)?;
    Ok(key)
}

/// Compose the key for one account's document, the shape `TryComposeForAccount` makes.
pub fn compose_for_account(directory: &str, steam_id: u64) -> Result<String, KeyRefusal> {
    compose(directory, &steam_id.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_five_documents_the_gamemode_actually_writes() {
        for key in [
            "characters/76561198000000000.json",
            "features.json",
            "laws.json",
            "permissions.json",
            "doors/minimal.json",
        ] {
            assert!(is_legal(key), "{key} should be legal");
        }
    }

    #[test]
    fn refuses_traversal() {
        assert_eq!(check("../secrets.json"), Err(KeyRefusal::RelativeSegment));
        assert_eq!(
            check("characters/../../etc/passwd"),
            Err(KeyRefusal::RelativeSegment)
        );
        assert_eq!(check("./features.json"), Err(KeyRefusal::RelativeSegment));
    }

    #[test]
    fn refuses_uppercase_rather_than_folding_it() {
        assert_eq!(
            check("Features.json"),
            Err(KeyRefusal::IllegalCharacter { character: 'F' })
        );
    }

    #[test]
    fn refuses_reserved_device_names_with_and_without_an_extension() {
        assert!(matches!(
            check("nul.json"),
            Err(KeyRefusal::ReservedDeviceName { .. })
        ));
        assert!(matches!(
            check("con"),
            Err(KeyRefusal::ReservedDeviceName { .. })
        ));
        assert!(matches!(
            check("com1.json"),
            Err(KeyRefusal::ReservedDeviceName { .. })
        ));
        assert!(matches!(
            check("lpt9.json"),
            Err(KeyRefusal::ReservedDeviceName { .. })
        ));
        // com0 is not a device; only 1-9 are.
        assert!(is_legal("com0.json"));
        // A five-character stem is not a port name.
        assert!(is_legal("com10.json"));
    }

    #[test]
    fn refuses_edge_separators_and_empty_segments() {
        assert_eq!(check("/features.json"), Err(KeyRefusal::LeadingSeparator));
        assert_eq!(check("characters/"), Err(KeyRefusal::TrailingSeparator));
        assert_eq!(check("characters//1.json"), Err(KeyRefusal::EmptySegment));
    }

    #[test]
    fn refuses_a_key_longer_than_the_column() {
        let key = format!("{}.json", "a".repeat(MAXIMUM_LENGTH));
        assert!(matches!(check(&key), Err(KeyRefusal::TooLong { .. })));
        // Exactly at the limit is legal, which is what makes the column width right.
        let key = "a".repeat(MAXIMUM_LENGTH);
        assert!(is_legal(&key));
    }

    #[test]
    fn refuses_the_characters_a_url_or_a_query_would_care_about() {
        for bad in [
            "a b.json",
            "a%2e.json",
            "a?b.json",
            "a\\b.json",
            "a'b.json",
            "a;b.json",
        ] {
            assert!(!is_legal(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn composes_the_account_shape() {
        assert_eq!(
            compose_for_account("characters", 76561198000000000).unwrap(),
            "characters/76561198000000000.json"
        );
    }

    #[test]
    fn directory_of_matches_the_csharp() {
        assert_eq!(directory_of("characters/1.json"), "characters");
        assert_eq!(directory_of("features.json"), "");
        assert_eq!(directory_of(""), "");
    }
}

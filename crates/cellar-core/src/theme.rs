//! The Applejack palette, in one place.
//!
//! AppleJackRP generates its colours from `Branding/palette.py` into
//! `Branding/tokens/applejack-tokens.json`, and its `BRANDING.md` is explicit
//! that a committed copy is a copy that goes stale. Cellar therefore states the
//! tokens once, here, with the role each one carries, and the web UI and the
//! TUI both derive from this rather than retyping hex.
//!
//! `cellar theme sync <path-to-applejack-tokens.json>` regenerates this file's
//! constants, so a palette change upstream is one command rather than a hunt.
//!
//! Applejack is blue, by standing rule. Azure against Frost.

use serde::{Deserialize, Serialize};

/// One colour, in both themes, with the job it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub name: &'static str,
    pub dark: &'static str,
    pub light: &'static str,
    pub role: &'static str,
}

macro_rules! token {
    ($ident:ident, $name:literal, $dark:literal, $light:literal, $role:literal) => {
        pub const $ident: Token = Token {
            name: $name,
            dark: $dark,
            light: $light,
            role: $role,
        };
    };
}

token!(
    AZURE,
    "azure",
    "#2F8FE0",
    "#175694",
    "Primary accent, actions, focus"
);
token!(
    AZURE_HOVER,
    "azure-hover",
    "#57A6E8",
    "#124A80",
    "Primary action, hovered"
);
token!(
    FROST,
    "frost",
    "#9CC4D4",
    "#457588",
    "Information, cold and inactive states"
);
token!(
    RUSSET,
    "russet",
    "#DA5B4D",
    "#A33326",
    "Wanted, crime, destructive actions"
);
token!(
    ORCHARD,
    "orchard",
    "#6FA862",
    "#47713C",
    "Lawful, safe, success"
);
token!(INK, "ink", "#0E0F11", "#0E0F11", "Deepest ground");
token!(SHELL, "shell", "#191B1E", "#F4F3F1", "Panel background");
token!(RAISED, "raised", "#212327", "#FFFFFF", "Card background");
token!(TEXT, "text", "#F2F2F0", "#201F1D", "Primary text");
token!(
    TEXT_MUTED,
    "text-muted",
    "#7A7D81",
    "#4A4642",
    "Secondary text"
);
token!(
    LOG_TRACE,
    "log-trace",
    "#626A78",
    "#697386",
    "Trace and low-signal output"
);
token!(LOG_DEBUG, "log-debug", "#9CC4D4", "#457588", "Debug output");
token!(
    LOG_INFO,
    "log-info",
    "#F2F2F0",
    "#201F1D",
    "Informational output"
);
token!(
    LOG_WARNING,
    "log-warning",
    "#D8B45A",
    "#8A650A",
    "Warnings and degraded service"
);
token!(
    LOG_ERROR,
    "log-error",
    "#DA5B4D",
    "#A33326",
    "Errors and failures"
);
token!(
    LOG_STORAGE,
    "log-storage",
    "#B58AF0",
    "#6B46A1",
    "Database and document storage"
);
token!(
    LOG_NETWORK,
    "log-network",
    "#68D7C4",
    "#287A6E",
    "Network, bridge and lobby"
);
token!(
    LOG_PLAYERS,
    "log-players",
    "#57A6E8",
    "#175694",
    "Players and identity"
);
token!(
    LOG_ENGINE,
    "log-engine",
    "#D8A657",
    "#8A5E0A",
    "Engine, map and physics"
);
token!(
    LOG_GAMEPLAY,
    "log-gameplay",
    "#F2A274",
    "#A45126",
    "Gamemode activity"
);
token!(
    LOG_CELLAR,
    "log-cellar",
    "#9CC4D4",
    "#457588",
    "Cellar supervisor activity"
);

/// Every token, in the order the palette documents them.
pub const TOKENS: &[Token] = &[
    AZURE,
    AZURE_HOVER,
    FROST,
    RUSSET,
    ORCHARD,
    INK,
    SHELL,
    RAISED,
    TEXT,
    TEXT_MUTED,
    LOG_TRACE,
    LOG_DEBUG,
    LOG_INFO,
    LOG_WARNING,
    LOG_ERROR,
    LOG_STORAGE,
    LOG_NETWORK,
    LOG_PLAYERS,
    LOG_ENGINE,
    LOG_GAMEPLAY,
    LOG_CELLAR,
];

/// The wordmark, as the TUI splash draws it.
pub const WORDMARK: &str = "A P P L E J A C K";

/// The mark: an apple in cross-section with its five-point seed star.
pub const STAR: &str = "★";

pub const TAGLINE: &str = "city roleplay for s&box";

/// Parse `#RRGGBB` into components, for the TUI which needs numbers.
pub const fn rgb(hex: &str) -> (u8, u8, u8) {
    let bytes = hex.as_bytes();
    // A const fn cannot return a Result, and every literal above is checked by
    // the test below, so an unparseable value is a compile-time-visible bug
    // rather than a runtime branch.
    let (r, g, b) = (
        hex_pair(bytes[1], bytes[2]),
        hex_pair(bytes[3], bytes[4]),
        hex_pair(bytes[5], bytes[6]),
    );
    (r, g, b)
}

const fn hex_pair(high: u8, low: u8) -> u8 {
    hex_digit(high) * 16 + hex_digit(low)
}

const fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}

/// Emit the palette as CSS custom properties for the web UI.
///
/// Both themes are written: bare `:root` carries light, and dark is redefined
/// under `prefers-color-scheme` and an explicit `[data-theme]`, so a colour
/// never has its only definition inside a media query.
pub fn css_variables() -> String {
    let mut out = String::from(":root{\n");
    for token in TOKENS {
        out.push_str(&format!("  --aj-{}: {};\n", token.name, token.light));
    }
    out.push_str("}\n@media (prefers-color-scheme: dark){:root:not([data-theme=\"light\"]){\n");
    for token in TOKENS {
        out.push_str(&format!("  --aj-{}: {};\n", token.name, token.dark));
    }
    out.push_str("}}\n:root[data-theme=\"dark\"]{\n");
    for token in TOKENS {
        out.push_str(&format!("  --aj-{}: {};\n", token.name, token.dark));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_token_is_a_parseable_six_digit_hex() {
        for token in TOKENS {
            for value in [token.dark, token.light] {
                assert_eq!(value.len(), 7, "{} {value}", token.name);
                assert!(value.starts_with('#'), "{} {value}", token.name);
                assert!(
                    value[1..].chars().all(|c| c.is_ascii_hexdigit()),
                    "{} {value}",
                    token.name
                );
            }
        }
    }

    #[test]
    fn rgb_reads_the_azure_the_brand_document_states() {
        assert_eq!(rgb(AZURE.dark), (0x2F, 0x8F, 0xE0));
        assert_eq!(rgb(RUSSET.dark), (0xDA, 0x5B, 0x4D));
        assert_eq!(rgb(ORCHARD.dark), (0x6F, 0xA8, 0x62));
    }

    #[test]
    fn css_defines_every_token_in_the_bare_root_as_well_as_dark() {
        let css = css_variables();
        for token in TOKENS {
            assert!(css.contains(&format!("--aj-{}: {}", token.name, token.light)));
            assert!(css.contains(&format!("--aj-{}: {}", token.name, token.dark)));
        }
        assert!(css.contains(":root{"));
        assert!(css.contains("prefers-color-scheme: dark"));
        assert!(css.contains("[data-theme=\"dark\"]"));
    }

    #[test]
    fn token_names_are_unique() {
        let mut names: Vec<&str> = TOKENS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }
}

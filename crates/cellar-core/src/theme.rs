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
    RUSSET_TEXT,
    "russet-text",
    "#E8776A",
    "#8E2A1F",
    "Destructive wording, on any ground"
);
token!(
    ORCHARD,
    "orchard",
    "#6FA862",
    "#47713C",
    "Lawful, safe, success"
);
// The light theme was dead for exactly one reason: `ink` carried the dark value
// in both themes, so the page painted `#201F1D` body text on a `#0E0F11` ground
// at 1.15:1. It is a light ground now, and depth runs the other way in the light
// theme: the page is the tinted one and cards are white, which is the ordinary
// shape and the one that keeps every token above 4.5:1.
token!(INK, "ink", "#0E0F11", "#F4F3F1", "Deepest ground");
token!(SHELL, "shell", "#191B1E", "#FAF9F8", "Panel background");

// The scrim behind a modal, and the colour a shadow is mixed from. Split out of
// `ink` when `ink` stopped being dark in both themes: a backdrop that lightens
// the page it covers is not a backdrop, so this stays dark either way. Not the
// same value twice, because a full-strength black veil over a light page reads
// as a rendering fault rather than as depth.
token!(
    SHADOW,
    "shadow",
    "#0E0F11",
    "#2A2724",
    "Modal scrim and shadows"
);
token!(RAISED, "raised", "#212327", "#FFFFFF", "Card background");
token!(TEXT, "text", "#F2F2F0", "#201F1D", "Primary text");
token!(
    TEXT_MUTED,
    "text-muted",
    "#8B8E93",
    "#4A4642",
    "Secondary text"
);
token!(
    LOG_TRACE,
    "log-trace",
    "#7C8496",
    // Darkened from #697386, which was 4.31:1 on the light console ground and
    // the only log token that missed AA there.
    "#5C6579",
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
    RUSSET_TEXT,
    ORCHARD,
    INK,
    SHADOW,
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

    /// WCAG 2.x relative luminance, from the sRGB definition.
    fn luminance(hex: &str) -> f64 {
        let (r, g, b) = rgb(hex);
        let channel = |raw: u8| {
            let c = f64::from(raw) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(foreground: &str, background: &str) -> f64 {
        let (a, b) = (luminance(foreground), luminance(background));
        let (high, low) = if a > b { (a, b) } else { (b, a) };
        (high + 0.05) / (low + 0.05)
    }

    /// Every ground either theme draws words on. `style.css:14` is `INK`,
    /// panels and headers are `SHELL`, cards are `RAISED`.
    const GROUNDS: &[Token] = &[INK, SHELL, RAISED];

    /// Both halves of every token, so the light theme is held to the same bar.
    ///
    /// It was not, and that is how it stayed broken: `ink` carried the dark
    /// value in both themes and no test looked at the light one, so the light
    /// theme rendered body text at 1.15:1 and shipped that way for months.
    #[test]
    fn body_text_tokens_meet_wcag_aa_on_every_ground_in_both_themes() {
        // RUSSET and AZURE_HOVER are absent deliberately: they are borders and
        // fills, where WCAG asks 3:1. RUSSET_TEXT exists so destructive wording
        // has a value that clears 4.5:1 on a card as well as on the shell.
        for token in [TEXT, TEXT_MUTED, RUSSET_TEXT, AZURE, FROST, ORCHARD] {
            for ground in GROUNDS {
                for (theme, ink, on) in [
                    ("dark", token.dark, ground.dark),
                    ("light", token.light, ground.light),
                ] {
                    let ratio = contrast(ink, on);
                    assert!(
                        ratio >= 4.5,
                        "{} on {} in {theme} is {ratio:.2}:1, below AA",
                        token.name,
                        ground.name
                    );
                }
            }
        }
    }

    #[test]
    fn log_category_tokens_are_legible_in_the_console_in_both_themes() {
        // The console is the only place these are drawn, and it is INK. Its
        // colour key shares that ground for the same reason: it was on the
        // card's `raised` ground, where trace and error fell to 4.2:1, and a
        // key drawn on a different ground from the thing it keys is wrong
        // anyway. At 12.5px they are normal-size text, so 4.5:1.
        for token in TOKENS.iter().filter(|t| t.name.starts_with("log-")) {
            for (theme, ink, ground) in [
                ("dark", token.dark, INK.dark),
                ("light", token.light, INK.light),
            ] {
                let ratio = contrast(ink, ground);
                assert!(
                    ratio >= 4.5,
                    "{} on ink in {theme} is {ratio:.2}:1, below AA",
                    token.name
                );
            }
        }
    }

    /// A scrim that lightens the page it covers is not a scrim.
    #[test]
    fn the_shadow_token_is_dark_in_both_themes() {
        for value in [SHADOW.dark, SHADOW.light] {
            let ratio = contrast(value, "#FFFFFF");
            assert!(
                ratio >= 7.0,
                "the scrim is {ratio:.2}:1 against white, which is not a scrim"
            );
        }
    }

    #[test]
    fn no_token_is_accidentally_identical_across_themes() {
        for token in TOKENS {
            assert_ne!(
                token.dark, token.light,
                "{} has one value doing two jobs",
                token.name
            );
        }
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

//! The Applejack palette, as terminal colours.
//!
//! Derived from `cellar_core::theme` rather than restated, so the TUI and the
//! web UI are the same blue. The dark variants are used: a terminal running a
//! dedicated server is a dark terminal.

use cellar_core::theme;
use ratatui::style::{Color, Modifier, Style};

fn colour(token: theme::Token) -> Color {
    let (r, g, b) = theme::rgb(token.dark);
    Color::Rgb(r, g, b)
}

pub fn azure() -> Color {
    colour(theme::AZURE)
}

pub fn frost() -> Color {
    colour(theme::FROST)
}

pub fn russet() -> Color {
    colour(theme::RUSSET)
}

pub fn orchard() -> Color {
    colour(theme::ORCHARD)
}

pub fn text() -> Color {
    colour(theme::TEXT)
}

pub fn muted() -> Color {
    colour(theme::TEXT_MUTED)
}

pub fn ground() -> Color {
    colour(theme::INK)
}

pub fn panel() -> Color {
    colour(theme::SHELL)
}

/// Panel borders and their titles.
pub fn border() -> Style {
    Style::default().fg(frost())
}

pub fn title() -> Style {
    Style::default().fg(frost()).add_modifier(Modifier::BOLD)
}

pub fn body() -> Style {
    Style::default().fg(text())
}

pub fn dim() -> Style {
    Style::default().fg(muted())
}

pub fn accent() -> Style {
    Style::default().fg(azure())
}

/// The colour a lifecycle state should read as.
pub fn state_colour(state: cellar_core::State) -> Color {
    use cellar_core::State;
    match state {
        State::Running => orchard(),
        State::Starting | State::Stopping | State::Backoff => frost(),
        State::Stopped | State::CrashLooping => russet(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_blue_is_the_brand_blue() {
        assert_eq!(azure(), Color::Rgb(0x2F, 0x8F, 0xE0));
    }

    #[test]
    fn a_healthy_server_is_orchard_and_a_crash_loop_is_russet() {
        assert_eq!(state_colour(cellar_core::State::Running), orchard());
        assert_eq!(state_colour(cellar_core::State::CrashLooping), russet());
        assert_ne!(
            state_colour(cellar_core::State::Running),
            state_colour(cellar_core::State::Stopped)
        );
    }
}

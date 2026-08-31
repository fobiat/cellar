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

fn token_colour(token: theme::Token) -> Color {
    colour(token)
}

pub fn log_colour(level: cellar_core::Level, who: &str, message: &str, local: bool) -> Color {
    if level == cellar_core::Level::Error {
        return token_colour(theme::LOG_ERROR);
    }
    if local {
        return token_colour(theme::LOG_CELLAR);
    }

    let value = format!("{who} {message}").to_ascii_lowercase();
    let category =
        if value.contains("storage") || value.contains("database") || value.contains("document") {
            theme::LOG_STORAGE
        } else if value.contains("network") || value.contains("connect") || value.contains("lobby")
        {
            theme::LOG_NETWORK
        } else if value.contains("player") || value.contains("identity") || value.contains("chat") {
            theme::LOG_PLAYERS
        } else if value.contains("physics") || value.contains("render") || value.contains("map") {
            theme::LOG_ENGINE
        } else if value.contains("applejack") || value.contains("game") {
            theme::LOG_GAMEPLAY
        } else {
            match level {
                cellar_core::Level::Trace => theme::LOG_TRACE,
                cellar_core::Level::Debug => theme::LOG_DEBUG,
                cellar_core::Level::Warning => theme::LOG_WARNING,
                cellar_core::Level::Info | cellar_core::Level::Error => theme::LOG_INFO,
            }
        };
    token_colour(category)
}

/// The colour a lifecycle state should read as.
pub fn state_colour(state: cellar_core::State) -> Color {
    use cellar_core::State;
    match state {
        State::Running => orchard(),
        State::Starting | State::Stopping | State::Backoff => frost(),
        // Alive but not serving. Warning rather than error: nothing has failed
        // yet, and the cause is as likely to be a wrong ready pattern as a
        // stuck engine.
        State::Unhealthy => token_colour(cellar_core::theme::LOG_WARNING),
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

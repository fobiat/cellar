# Cellar branding

Cellar owns its product name and wordmark. The logo mark is the AppleJack
Framework mug mark, used here as the shared visual link between the framework
and the server manager.

## Assets

- `crates/cellar-server/src/ui/assets/cellar-icon.svg` is the canonical web
  mark.
- `crates/cellar-server/src/ui/assets/cellar-icon.ico` is the Windows tray and
  installer mark.
- `crates/cellar-server/src/ui/assets/cellar-logo-horizontal.svg` and its
  `-dark` variant are repository and release-package wordmarks.
- `crates/cellar-server/src/ui/assets/favicon.svg` and the web manifest point
  at the canonical icon.

The browser UI, TUI, Discord notifications, Linux tray, Windows tray, favicons,
and release packages use these assets or their terminal-safe fallback. The
product name remains Cellar in every surface. AppleJack Framework appears only
when describing the integration or the shared visual language.

## Palette

The colours are defined in `crates/cellar-core/src/theme.rs`, using the
AppleJack Framework palette roles. UI code should consume those theme tokens,
not add a second set of hex values.

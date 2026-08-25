//! Reading, writing and capturing the server's configuration.
//!
//! Three different things share one screen, because to an operator they are one
//! question ("what is this server set to?") even though the engine keeps them
//! apart:
//!
//! - **Features**, 41 switches over whole gamemode systems, listed by
//!   `applejack_features` and written by `applejack_feature_set`.
//! - **Settings**, 7 catalogued gameplay values with bounds and a documented
//!   source, listed by `applejack_settings` and written by
//!   `applejack_setting_set`.
//! - **Convars**, the engine's own, discovered with `find`.
//!
//! A [`Snapshot`] is all three captured together, and it serialises to TOML or
//! YAML so a server's configuration can be committed, diffed and re-applied
//! somewhere else. That is the point: today the only record of how a server is
//! configured is the server.
//!
//! The two gamemode listings are parsed from formats read out of
//! `FeatureDirector.cs` and `SettingDirector.cs`, so they are exact. The engine's
//! `find` output is not documented anywhere this could be read from, so that
//! parser is deliberately tolerant and marks what it could not understand
//! rather than guessing.

use serde::{Deserialize, Serialize};

/// Whether a feature may be changed, and when it takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Toggle {
    /// Not toggleable. The game is not recognisably Applejack without it.
    Core,
    /// Read once at boot. Changing it needs a restart.
    Boot,
    /// May flip mid-session.
    Live,
    /// The listing said something this build does not know.
    Unknown,
}

impl Toggle {
    fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "core" => Self::Core,
            "boot" => Self::Boot,
            "live" => Self::Live,
            _ => Self::Unknown,
        }
    }

    /// Whether writing this is worth attempting.
    pub fn is_writable(self) -> bool {
        !matches!(self, Self::Core)
    }
}

/// One row of `applejack_features`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub enabled: bool,
    /// True when the current state is the catalogue's default.
    pub is_default: bool,
    pub toggle: Toggle,
    pub title: String,
}

/// One row of `applejack_settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    pub id: String,
    pub value: String,
    pub default: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Setting {
    pub fn is_default(&self) -> bool {
        self.value == self.default
    }
}

/// One engine convar, as `find` reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Convar {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// Everything the server is set to, at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<Feature>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Setting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convars: Vec<Convar>,
}

impl Snapshot {
    pub fn feature(&self, id: &str) -> Option<&Feature> {
        self.features.iter().find(|feature| feature.id == id)
    }

    pub fn setting(&self, id: &str) -> Option<&Setting> {
        self.settings.iter().find(|setting| setting.id == id)
    }

    /// Only what an operator changed away from the catalogue's default.
    ///
    /// The useful thing to commit: a full dump is mostly defaults, and a diff
    /// against one is noise. This is the shape that answers "what is different
    /// about this server".
    pub fn overrides_only(&self) -> Self {
        Self {
            captured_at: self.captured_at.clone(),
            hostname: self.hostname.clone(),
            features: self
                .features
                .iter()
                .filter(|feature| !feature.is_default)
                .cloned()
                .collect(),
            settings: self
                .settings
                .iter()
                .filter(|setting| !setting.is_default())
                .cloned()
                .collect(),
            convars: self.convars.clone(),
        }
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn to_yaml(&self) -> Result<String, String> {
        serde_yaml_ng::to_string(self).map_err(|e| e.to_string())
    }

    /// Read a snapshot from TOML or YAML, deciding by shape rather than by
    /// extension: a file renamed by hand should still load.
    pub fn parse(text: &str) -> Result<Self, String> {
        let toml_error = match toml::from_str::<Self>(text) {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) => error.to_string(),
        };

        serde_yaml_ng::from_str::<Self>(text)
            .map_err(|yaml_error| format!("not TOML ({toml_error}) and not YAML ({yaml_error})"))
    }
}

/// One difference between a captured snapshot and a desired one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub from: String,
    pub to: String,
    /// The console command that would make it so.
    pub command: String,
    /// Set when the change cannot be applied, with the reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// True when the change only takes effect after a restart.
    pub needs_restart: bool,
}

/// What it would take to move `current` to `desired`.
///
/// Only what `desired` actually mentions. A snapshot with three features in it
/// is a request to set those three, not a request to reset everything else to
/// its default, because the second reading turns a partial file into a way to
/// wipe a server's configuration.
pub fn plan(current: &Snapshot, desired: &Snapshot) -> Vec<Change> {
    let mut changes = Vec::new();

    for wanted in &desired.features {
        let Some(existing) = current.feature(&wanted.id) else {
            changes.push(Change {
                id: wanted.id.clone(),
                from: "absent".to_owned(),
                to: state_word(wanted.enabled).to_owned(),
                command: feature_command(&wanted.id, wanted.enabled),
                refused: Some("this server has no feature with that id".to_owned()),
                needs_restart: false,
            });
            continue;
        };

        if existing.enabled == wanted.enabled {
            continue;
        }

        changes.push(Change {
            id: wanted.id.clone(),
            from: state_word(existing.enabled).to_owned(),
            to: state_word(wanted.enabled).to_owned(),
            command: feature_command(&wanted.id, wanted.enabled),
            // The live server's own catalogue decides, not the file's: a file
            // could claim a core feature is toggleable and it would still be
            // refused by the gamemode.
            refused: (!existing.toggle.is_writable())
                .then(|| "this feature is core and cannot be toggled".to_owned()),
            needs_restart: existing.toggle == Toggle::Boot,
        });
    }

    for wanted in &desired.settings {
        let Some(existing) = current.setting(&wanted.id) else {
            changes.push(Change {
                id: wanted.id.clone(),
                from: "absent".to_owned(),
                to: wanted.value.clone(),
                command: setting_command(&wanted.id, &wanted.value),
                refused: Some("this server has no setting with that id".to_owned()),
                needs_restart: false,
            });
            continue;
        };

        if existing.value == wanted.value {
            continue;
        }

        changes.push(Change {
            id: wanted.id.clone(),
            from: existing.value.clone(),
            to: wanted.value.clone(),
            command: setting_command(&wanted.id, &wanted.value),
            refused: None,
            needs_restart: false,
        });
    }

    changes
}

fn state_word(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

/// The command that sets a feature.
pub fn feature_command(id: &str, enabled: bool) -> String {
    format!("applejack_feature_set {id} {}", state_word(enabled))
}

/// The command that sets a catalogued setting.
pub fn setting_command(id: &str, value: &str) -> String {
    format!("applejack_setting_set {id} {value}")
}

/// Parse the output of `applejack_features`.
///
/// The format is fixed-width, from `FeatureDirector.cs`:
///
/// ```text
/// {id,-28} {state,-36} {toggle,-5} {title}
/// ```
///
/// where state reads `on (default off)`. Parsed by splitting on whitespace
/// rather than by column offset: a long id pushes the columns right, and a
/// fixed-offset reader would then silently read the wrong field.
pub fn parse_features(lines: &[String]) -> Vec<Feature> {
    let mut features = Vec::new();

    for line in lines {
        let line = line.trim();
        // The command prefixes its own summary line, which is not a row.
        if line.is_empty() || line.starts_with('[') {
            continue;
        }

        let Some((id, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };

        // An id is dotted and lowercase; anything else is prose.
        if !id.contains('.')
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_' || c == '-')
        {
            continue;
        }

        let rest = rest.trim_start();
        let Some(state) = rest.split_whitespace().next() else {
            continue;
        };

        let enabled = match state.to_ascii_lowercase().as_str() {
            "on" | "true" | "enabled" => true,
            "off" | "false" | "disabled" => false,
            _ => continue,
        };

        // `(default off)` follows the state.
        let default_enabled = rest
            .split_once("default")
            .map(|(_, after)| after.trim_start().starts_with("on"))
            .unwrap_or(enabled);

        // After the closing paren come the toggle class and the title.
        let tail = match rest.split_once(')') {
            Some((_, tail)) => tail.trim_start(),
            None => rest,
        };

        let mut words = tail.split_whitespace();
        let toggle = words.next().map(Toggle::parse).unwrap_or(Toggle::Unknown);
        let title = words.collect::<Vec<_>>().join(" ");

        features.push(Feature {
            id: id.to_owned(),
            enabled,
            is_default: enabled == default_enabled,
            toggle,
            title,
        });
    }

    features
}

/// Parse the output of `applejack_settings`.
///
/// From `SettingDirector.cs`:
///
/// ```text
/// {id,-34} {value,-10} default {default,-10} {bounds,-18} {source}
/// ```
pub fn parse_settings(lines: &[String]) -> Vec<Setting> {
    let mut settings = Vec::new();

    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }

        let mut words = line.split_whitespace();
        let Some(id) = words.next() else { continue };

        if !id.contains('.') {
            continue;
        }

        let Some(value) = words.next() else { continue };

        // The literal word `default` separates the value from its default.
        let rest: Vec<&str> = words.collect();
        let Some(marker) = rest.iter().position(|word| *word == "default") else {
            continue;
        };

        let default = rest.get(marker + 1).copied().unwrap_or_default().to_owned();
        let remainder = &rest[(marker + 2).min(rest.len())..];

        // Bounds look like `[0, 600]` or `0..600`; anything else is the source.
        let (bounds, source) = match remainder.first() {
            Some(first) if first.starts_with('[') || first.contains("..") => (
                Some((*first).to_owned()),
                Some(remainder[1..].join(" ")).filter(|s| !s.is_empty()),
            ),
            _ => (None, Some(remainder.join(" ")).filter(|s| !s.is_empty())),
        };

        settings.push(Setting {
            id: id.to_owned(),
            value: value.to_owned(),
            default,
            bounds,
            source,
        });
    }

    settings
}

/// Parse the output of the engine's `find`.
///
/// Deliberately tolerant. Unlike the two above, this format could not be read
/// from source, so it accepts `name value`, `name : help` and `name = value`
/// and records what it could not classify as a bare name rather than dropping
/// the row or inventing a value.
pub fn parse_convars(lines: &[String]) -> Vec<Convar> {
    let mut convars = Vec::new();

    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }

        let Some(name) = line.split_whitespace().next() else {
            continue;
        };

        // A convar name is one token of identifier characters. Prose is not.
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
        {
            continue;
        }

        let rest = line[name.len()..].trim();

        let (value, help) = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim();
            match after.split_once(char::is_whitespace) {
                Some((value, help)) => (Some(value.to_owned()), Some(help.trim().to_owned())),
                None => (Some(after.to_owned()), None),
            }
        } else if let Some(after) = rest.strip_prefix(':') {
            (None, Some(after.trim().to_owned()))
        } else if rest.is_empty() {
            (None, None)
        } else {
            match rest.split_once(char::is_whitespace) {
                Some((value, help)) => (Some(value.to_owned()), Some(help.trim().to_owned())),
                None => (Some(rest.to_owned()), None),
            }
        };

        convars.push(Convar {
            name: name.to_owned(),
            value,
            help: help.filter(|h| !h.is_empty()),
        });
    }

    convars
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    /// The exact shape `FeatureDirector.cs` prints.
    #[test]
    fn it_reads_the_feature_listing() {
        let output = lines(
            "[Features] 41 feature(s)\n\
             ui.menu.admin                off (default off)                    live  Admin panel\n\
             ui.menu.devmenu              on (default off)                     live  Developer menu\n\
             economy.manufacture          on (default on)                      core  Manufacture\n\
             world.daynight               on (default on)                      boot  Day and night",
        );

        let features = parse_features(&output);
        assert_eq!(features.len(), 4, "{features:#?}");

        assert_eq!(features[0].id, "ui.menu.admin");
        assert!(!features[0].enabled);
        assert!(features[0].is_default);
        assert_eq!(features[0].toggle, Toggle::Live);
        assert_eq!(features[0].title, "Admin panel");

        // Turned on where the default is off: this is an override.
        assert!(features[1].enabled);
        assert!(!features[1].is_default);

        assert_eq!(features[2].toggle, Toggle::Core);
        assert!(!features[2].toggle.is_writable());

        assert_eq!(features[3].toggle, Toggle::Boot);
    }

    #[test]
    fn the_summary_line_is_not_a_feature() {
        let features = parse_features(&lines("[Features] 41 feature(s)"));
        assert!(features.is_empty());
    }

    /// A long id pushes every later column right. A fixed-offset reader would
    /// read the wrong field and report a wrong state, silently.
    #[test]
    fn a_long_id_does_not_shift_the_reader_onto_the_wrong_column() {
        let features = parse_features(&lines(
            "a.very.long.feature.identifier.that.overflows off (default on) live A title",
        ));

        assert_eq!(features.len(), 1);
        assert!(!features[0].enabled);
        assert!(!features[0].is_default);
        assert_eq!(features[0].toggle, Toggle::Live);
        assert_eq!(features[0].title, "A title");
    }

    #[test]
    fn it_reads_the_settings_listing() {
        let output = lines(
            "[Settings] a value takes effect where its call site reads it\n\
             combat.respawn_wait_seconds        30         default 30         [0, 600]           sh_config.lua Spawn Time\n\
             crime.arrest_seconds               240        default 300        [0, 3600]          sh_config.lua Arrest Time",
        );

        let settings = parse_settings(&output);
        assert_eq!(settings.len(), 2, "{settings:#?}");

        assert_eq!(settings[0].id, "combat.respawn_wait_seconds");
        assert_eq!(settings[0].value, "30");
        assert_eq!(settings[0].default, "30");
        assert!(settings[0].is_default());
        assert_eq!(settings[0].bounds.as_deref(), Some("[0,"));

        assert_eq!(settings[1].value, "240");
        assert!(!settings[1].is_default(), "240 is not the default 300");
    }

    #[test]
    fn convar_parsing_tolerates_the_shapes_it_might_meet() {
        let convars = parse_convars(&lines(
            "sv_cheats 0\n\
             net_hide_address = 1  hide the server address\n\
             net_debug : whether to log networking\n\
             hostname",
        ));

        assert_eq!(convars.len(), 4, "{convars:#?}");
        assert_eq!(convars[0].value.as_deref(), Some("0"));
        assert_eq!(convars[1].value.as_deref(), Some("1"));
        assert_eq!(convars[1].help.as_deref(), Some("hide the server address"));
        assert_eq!(convars[2].value, None);
        assert_eq!(
            convars[2].help.as_deref(),
            Some("whether to log networking")
        );
        assert_eq!(convars[3].value, None);
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            captured_at: Some("2026-08-25T10:00:00Z".to_owned()),
            hostname: Some("AppleJackRP Dev".to_owned()),
            features: vec![
                Feature {
                    id: "ui.menu.admin".into(),
                    enabled: false,
                    is_default: true,
                    toggle: Toggle::Live,
                    title: "Admin panel".into(),
                },
                Feature {
                    id: "economy.manufacture".into(),
                    enabled: true,
                    is_default: true,
                    toggle: Toggle::Core,
                    title: "Manufacture".into(),
                },
                Feature {
                    id: "world.daynight".into(),
                    enabled: true,
                    is_default: true,
                    toggle: Toggle::Boot,
                    title: "Day and night".into(),
                },
            ],
            settings: vec![Setting {
                id: "crime.arrest_seconds".into(),
                value: "300".into(),
                default: "300".into(),
                bounds: None,
                source: None,
            }],
            convars: Vec::new(),
        }
    }

    #[test]
    fn a_snapshot_round_trips_through_toml_and_yaml() {
        let original = snapshot();

        let toml_text = original.to_toml().unwrap();
        assert_eq!(Snapshot::parse(&toml_text).unwrap(), original);

        let yaml_text = original.to_yaml().unwrap();
        assert_eq!(Snapshot::parse(&yaml_text).unwrap(), original);
    }

    #[test]
    fn parse_reads_either_format_regardless_of_what_it_is_called() {
        let yaml = "features:\n  - id: ui.menu.admin\n    enabled: true\n    is_default: false\n    toggle: live\n    title: Admin panel\n";
        let parsed = Snapshot::parse(yaml).unwrap();
        assert_eq!(parsed.features[0].id, "ui.menu.admin");
    }

    #[test]
    fn a_file_that_is_neither_says_so_about_both() {
        let error = Snapshot::parse("\t\x00 not a config at all {[").unwrap_err();
        assert!(error.contains("not TOML"), "{error}");
        assert!(error.contains("not YAML"), "{error}");
    }

    #[test]
    fn overrides_only_keeps_what_was_changed() {
        let mut current = snapshot();
        current.features[0].enabled = true;
        current.features[0].is_default = false;
        current.settings[0].value = "600".into();

        let overrides = current.overrides_only();
        assert_eq!(overrides.features.len(), 1);
        assert_eq!(overrides.features[0].id, "ui.menu.admin");
        assert_eq!(overrides.settings.len(), 1);
    }

    #[test]
    fn planning_produces_the_commands_that_would_apply_it() {
        let current = snapshot();

        let mut desired = Snapshot::default();
        desired.features.push(Feature {
            id: "ui.menu.admin".into(),
            enabled: true,
            is_default: false,
            toggle: Toggle::Live,
            title: String::new(),
        });
        desired.settings.push(Setting {
            id: "crime.arrest_seconds".into(),
            value: "600".into(),
            default: "300".into(),
            bounds: None,
            source: None,
        });

        let changes = plan(&current, &desired);
        assert_eq!(changes.len(), 2, "{changes:#?}");

        assert_eq!(changes[0].command, "applejack_feature_set ui.menu.admin on");
        assert_eq!(changes[0].from, "off");
        assert!(changes[0].refused.is_none());

        assert_eq!(
            changes[1].command,
            "applejack_setting_set crime.arrest_seconds 600"
        );
    }

    /// A partial file must be a request to set what it names, never a request
    /// to reset everything it does not.
    #[test]
    fn a_partial_snapshot_does_not_reset_what_it_omits() {
        let current = snapshot();
        let desired = Snapshot::default();
        assert!(plan(&current, &desired).is_empty());
    }

    #[test]
    fn nothing_to_do_produces_no_changes() {
        let current = snapshot();
        assert!(plan(&current, &current).is_empty());
    }

    #[test]
    fn a_core_feature_is_planned_but_marked_refused() {
        let current = snapshot();

        let mut desired = Snapshot::default();
        desired.features.push(Feature {
            id: "economy.manufacture".into(),
            enabled: false,
            is_default: false,
            toggle: Toggle::Live, // the file lies; the live catalogue decides
            title: String::new(),
        });

        let changes = plan(&current, &desired);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].refused.as_deref().unwrap().contains("core"));
    }

    #[test]
    fn a_boot_feature_is_flagged_as_needing_a_restart() {
        let current = snapshot();

        let mut desired = Snapshot::default();
        desired.features.push(Feature {
            id: "world.daynight".into(),
            enabled: false,
            is_default: false,
            toggle: Toggle::Boot,
            title: String::new(),
        });

        let changes = plan(&current, &desired);
        assert!(changes[0].needs_restart);
        assert!(
            changes[0].refused.is_none(),
            "it can be set, it just needs a restart"
        );
    }

    #[test]
    fn an_id_this_server_does_not_have_is_refused_by_name() {
        let current = snapshot();

        let mut desired = Snapshot::default();
        desired.features.push(Feature {
            id: "not.a.real.feature".into(),
            enabled: true,
            is_default: false,
            toggle: Toggle::Live,
            title: String::new(),
        });

        let changes = plan(&current, &desired);
        assert!(
            changes[0]
                .refused
                .as_deref()
                .unwrap()
                .contains("no feature")
        );
    }
}

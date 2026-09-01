//! What a gamemode tells Cellar about itself.
//!
//! Four places used to assume AppleJackRP: the `ready_pattern` default, the log
//! category heuristic, a doctor check that grepped one C# file by path, and
//! thirteen hardcoded command chips in the web UI. Each is now a line in this
//! table, and a gamemode Cellar has never heard of gets all four by writing
//! twenty lines of TOML.
//!
//! Deliberately small, and it must stay that way. This is not a Pterodactyl
//! egg. No install script, no config-rewrite language, no per-gamemode UI
//! layout. The boundary is: a profile describes a gamemode, it does not
//! configure one.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A gamemode's declarative description of itself.
///
/// Every field is optional. A profile that sets only `ready_pattern` is a
/// legitimate profile, and it is the one that fixes the defect this type was
/// written for: `facepunch.sandbox` failing readiness forever because it never
/// logs AppleJackRP's line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GamemodeProfile {
    /// Shown in the UI. Not an identifier: nothing routes on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// The log line that means "serving".
    ///
    /// A substring match, not a regex, matching what `grammar::classify` does
    /// with it. `server.ready_pattern` still overrides this per instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_pattern: Option<String>,

    /// The prefix this gamemode's convars share, without a trailing underscore.
    ///
    /// Drives `find <prefix>` in the palette and the log category heuristic,
    /// which used to test for the literal `applejack`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub convar_prefix: Option<String>,

    /// Console commands worth offering as one click.
    ///
    /// `[[profile.command]]` in the file. Named singular there because that is
    /// how a TOML array of tables reads at the point of use.
    #[serde(
        default,
        rename = "command",
        skip_serializing_if = "Vec::is_empty",
        alias = "commands"
    )]
    pub commands: Vec<ProfileCommand>,

    /// Map package idents this gamemode ships or supports.
    ///
    /// A map is a package, `org.name`, passed as the optional second positional
    /// argument to `+game`. There is no `+map` switch. Declaring them turns a
    /// typo'd `server.map` from a server that starts and never becomes ready
    /// into a `cellar doctor` failure naming the maps that do exist.
    ///
    /// Empty means "this gamemode did not say", never "this gamemode has no
    /// maps", so an empty list checks nothing.
    #[serde(
        default,
        rename = "map",
        skip_serializing_if = "Vec::is_empty",
        alias = "maps"
    )]
    pub maps: Vec<String>,

    /// Source-tree assertions `cellar doctor` should make.
    #[serde(
        default,
        rename = "check",
        skip_serializing_if = "Vec::is_empty",
        alias = "checks"
    )]
    pub checks: Vec<ProfileCheck>,
}

/// One entry in the command palette.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCommand {
    /// What the operator reads.
    pub label: String,

    /// What gets sent to the server's console, verbatim.
    pub command: String,

    /// Free-text heading the palette groups by. Ungrouped entries sort last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    /// Ask before running it.
    ///
    /// The profile's own judgement about its commands: Cellar cannot know that
    /// `applejack_wipe` is destructive and the gamemode can say so.
    #[serde(default)]
    pub confirm: bool,
}

/// A file in the gamemode's source tree that must contain given strings.
///
/// Resolved relative to the directory holding `server.project`, which is what
/// the AppleJackRP check it replaces did. A profile cannot name an absolute
/// path, and cannot look outside that tree: a config file is not a licence to
/// read arbitrary host files back through `cellar doctor`'s output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCheck {
    /// Reported as `gamemode: <name>`.
    pub name: String,

    /// Relative to the project directory. Forward slashes on every platform.
    pub file: PathBuf,

    /// Every one of these must appear in the file. Empty means the file must
    /// merely exist.
    #[serde(default)]
    pub contains: Vec<String>,

    /// What goes wrong when the check fails. This is the whole value of the
    /// check: "missing" tells an operator nothing they can act on.
    pub reason: String,
}

/// Why a profile was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileError(pub String);

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl GamemodeProfile {
    /// Refuse a profile that would misbehave at the point of use rather than at
    /// the point of parse.
    pub fn validate(&self) -> Result<(), ProfileError> {
        if let Some(prefix) = &self.convar_prefix {
            let usable = !prefix.is_empty()
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
            if !usable {
                return Err(ProfileError(format!(
                    "profile.convar_prefix '{prefix}' must be lowercase letters, digits and \
                     underscores: it is pasted into a console command and matched against log text"
                )));
            }
        }

        for command in &self.commands {
            if command.command.trim().is_empty() {
                return Err(ProfileError(format!(
                    "profile command '{}' has an empty command",
                    command.label
                )));
            }
            if command.label.trim().is_empty() {
                return Err(ProfileError(format!(
                    "the profile command '{}' has no label, so the palette would show a blank \
                     button",
                    command.command
                )));
            }
        }

        for check in &self.checks {
            // A check is read by `cellar doctor` and its path is printed. A
            // profile that could name `/etc/shadow` and assert what it contains
            // turns a config file into an oracle over the host filesystem.
            if check.file.is_absolute()
                || check
                    .file
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(ProfileError(format!(
                    "profile check '{}' names '{}': a check path must be relative to the project \
                     directory and may not climb out of it",
                    check.name,
                    check.file.display()
                )));
            }
            if check.reason.trim().is_empty() {
                return Err(ProfileError(format!(
                    "profile check '{}' has no reason, so a failure would tell the operator \
                     nothing they can act on",
                    check.name
                )));
            }
        }

        Ok(())
    }

    /// True when nothing was declared, so callers can tell "no profile" from
    /// "a profile that happens to be quiet".
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Which bucket of the console's category filter a log line belongs to.
    ///
    /// Lives here rather than in the server crate because the only thing that
    /// made it gamemode-specific was a test for the literal `applejack`, which
    /// is now `convar_prefix`. A gamemode that declares no prefix loses nothing:
    /// every other rule is about the engine, which is the same for all of them.
    ///
    /// Order matters. The first match wins, and the arms are ordered from the
    /// most specific subject to the least, so a line about a player's document
    /// counts as storage rather than players.
    pub fn category(&self, tag: &str, message: &str) -> Category {
        let text = format!("{tag} {message}").to_ascii_lowercase();
        let mentions = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));

        if mentions(&["storage", "database", "document"]) {
            Category::Storage
        } else if mentions(&["network", "connect", "lobby"]) {
            Category::Network
        } else if mentions(&["player", "identity", "chat"]) {
            Category::Players
        } else if mentions(&["physics", "render", "map"]) {
            Category::Engine
        } else if self
            .convar_prefix
            .as_deref()
            .is_some_and(|prefix| text.contains(prefix))
            || text.contains("game")
        {
            Category::Gameplay
        } else if text.contains("cellar") {
            Category::Cellar
        } else {
            Category::Other
        }
    }
}

/// The console's category filter, as a closed set.
///
/// An enum rather than a `String` because the browser renders one checkbox per
/// category and a typo on either side silently produces a filter that matches
/// nothing. The wire form is the lowercase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Cellar,
    Engine,
    Gameplay,
    Network,
    Players,
    Storage,
    Other,
}

impl Category {
    /// Every category, in the order the console lists them.
    pub const ALL: [Category; 7] = [
        Category::Cellar,
        Category::Engine,
        Category::Gameplay,
        Category::Network,
        Category::Players,
        Category::Storage,
        Category::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Cellar => "cellar",
            Category::Engine => "engine",
            Category::Gameplay => "gameplay",
            Category::Network => "network",
            Category::Players => "players",
            Category::Storage => "storage",
            Category::Other => "other",
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn profile(toml: &str) -> GamemodeProfile {
        toml::from_str(toml).expect("profile parses")
    }

    #[test]
    fn an_array_of_tables_reads_as_commands_and_checks() {
        let parsed = profile(
            r#"
            name = "AppleJackRP"
            ready_pattern = "Lobby created - session is joinable"
            convar_prefix = "applejack"

            [[command]]
            label = "List features"
            command = "applejack_features"

            [[check]]
            name = "spawn validation"
            file = "Code/Characters/CharacterDirector.cs"
            contains = ["GroundedOrAuthored"]
            reason = "players spawn inside geometry without it"
            "#,
        );

        assert_eq!(parsed.name.as_deref(), Some("AppleJackRP"));
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].command, "applejack_features");
        assert!(!parsed.commands[0].confirm);
        assert_eq!(parsed.checks.len(), 1);
        parsed.validate().expect("valid");
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let refused = toml::from_str::<GamemodeProfile>(r#"ready_patern = "x""#);
        assert!(refused.is_err(), "deny_unknown_fields must apply");
    }

    /// The whole point of the type. A profile that says only this is enough to
    /// stop `facepunch.sandbox` failing readiness forever.
    #[test]
    fn a_ready_pattern_alone_is_a_valid_profile() {
        let parsed = profile(r#"ready_pattern = "Connected to Steam""#);
        assert!(!parsed.is_empty());
        assert!(parsed.commands.is_empty());
        parsed.validate().expect("valid");
    }

    #[test]
    fn a_check_may_not_climb_out_of_the_project_directory() {
        let escaping = profile(
            r#"
            [[check]]
            name = "sneaky"
            file = "../../../../etc/shadow"
            reason = "no"
            "#,
        );
        let message = escaping.validate().expect_err("refused").0;
        assert!(message.contains("climb out"), "{message}");

        let absolute = profile(
            r#"
            [[check]]
            name = "sneaky"
            file = "/etc/shadow"
            reason = "no"
            "#,
        );
        assert!(absolute.validate().is_err());
    }

    #[test]
    fn a_convar_prefix_that_could_not_be_typed_is_refused() {
        let parsed = profile(r#"convar_prefix = "Apple Jack""#);
        assert!(parsed.validate().is_err());
        assert!(profile(r#"convar_prefix = "applejack""#).validate().is_ok());
        assert!(profile(r#"convar_prefix = "sbox_2""#).validate().is_ok());
    }

    #[test]
    fn a_check_without_a_reason_is_refused() {
        let parsed = profile(
            r#"
            [[check]]
            name = "spawn validation"
            file = "Code/Thing.cs"
            reason = "  "
            "#,
        );
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn a_command_with_no_label_would_render_a_blank_button() {
        let parsed = profile(
            r#"
            [[command]]
            label = ""
            command = "applejack_features"
            "#,
        );
        assert!(parsed.validate().is_err());
    }

    /// The defect this replaced: `logs.rs` tested for the literal `applejack`,
    /// so a gamemode with any other convar prefix had all its own chatter
    /// bucketed as "other".
    #[test]
    fn a_gamemode_line_is_gameplay_only_for_the_declared_prefix() {
        let applejack = profile(r#"convar_prefix = "applejack""#);
        let sandbox = profile(r#"convar_prefix = "sbox""#);

        assert_eq!(
            applejack.category("AppleJack", "spawned a citizen"),
            Category::Gameplay
        );
        assert_eq!(
            sandbox.category("AppleJack", "spawned a citizen"),
            Category::Other
        );
        assert_eq!(sandbox.category("Sbox", "tool used"), Category::Gameplay);
    }

    /// Every rule except the prefix one is about the engine, so a profile-free
    /// config categorises exactly as it did before profiles existed.
    #[test]
    fn the_engine_rules_do_not_need_a_profile() {
        let bare = GamemodeProfile::default();
        assert_eq!(bare.category("Storage", "wrote a doc"), Category::Storage);
        assert_eq!(bare.category("Bootstrap", "Lobby up"), Category::Network);
        assert_eq!(bare.category("Cellar", "started"), Category::Cellar);
        assert_eq!(bare.category("Render", "map loaded"), Category::Engine);
        assert_eq!(bare.category("Whatever", "hello"), Category::Other);
    }

    #[test]
    fn a_category_round_trips_through_its_wire_form() {
        for category in Category::ALL {
            let json = serde_json::to_string(&category).expect("serialises");
            assert_eq!(json, format!("\"{}\"", category.as_str()));
            let back: Category = serde_json::from_str(&json).expect("parses");
            assert_eq!(back, category);
        }
    }

    #[test]
    fn the_default_profile_is_empty() {
        assert!(GamemodeProfile::default().is_empty());
        assert!(GamemodeProfile::default().validate().is_ok());
    }
}

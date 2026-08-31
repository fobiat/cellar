//! Building the command line, and nothing else.
//!
//! Pure, and tested, because every argument here is a decision with a reason and
//! several of them are corrections to what the project does today.

use std::path::Path;

use cellar_core::config::{Launcher, ServerConfig};
use cellar_core::secret::Secret;

/// A command ready to spawn, and a copy of it safe to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

impl Command {
    /// The command as one line, with the GSLT replaced by asterisks.
    ///
    /// Every path that prints a command goes through this. The token is the one
    /// value on this line that must never reach a log, a webhook or a terminal
    /// somebody screenshots.
    pub fn redacted(&self, gslt: Option<&Secret>) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(self.program.clone());

        let mut mask_next = false;
        for arg in &self.args {
            if mask_next {
                parts.push("***".to_owned());
                mask_next = false;
                continue;
            }
            mask_next = arg == "+net_game_server_token";
            parts.push(arg.clone());
        }

        let line = parts.join(" ");
        match gslt {
            Some(secret) => cellar_core::secret::redact_all(&line, &[secret]),
            None => line,
        }
    }
}

/// Compose the launch command for a config.
pub fn command_for(config: &ServerConfig, bridge_needs_local_http: bool) -> Command {
    let executable = path_string(&config.executable);

    let (program, mut args) = match config.launcher {
        Launcher::Native => (executable, Vec::new()),
        // Facepunch's Linux dedicated server ships on the public branch and
        // does not link: `ErrorReports::Breadcrumb` is referenced by
        // libengine2, librendersystemvulkan and libtier0, and defined by none
        // of the 33 shipped libraries. Stated this way on purpose. Anyone who
        // notices that depot 1892933 has a public manifest will otherwise
        // conclude the Wine decision was a mistake and reverse it. Worth a
        // re-test each release; it is one symbol away from working.
        Launcher::Wine => ("wine".to_owned(), vec![executable]),
    };

    // `+game`, not `+project`. `+project` loads the project's metadata and then
    // idles at the bare console without ever booting a map; `+game` with a local
    // .sbproj compiles it and loads the project's own default scene.
    args.push("+game".to_owned());
    args.push(
        config
            .game
            .as_deref()
            .filter(|game| !game.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| path_string(&config.project)),
    );

    if let Some(map) = &config.map
        && !map.trim().is_empty()
    {
        args.push(map.clone());
    }

    args.push("+hostname".to_owned());
    args.push(config.hostname.clone());

    // Deliberately no `+maxplayers`. There is no such convar or launch switch in
    // the engine: `LaunchArguments.MaxPlayers` exists but nothing on the command
    // line sets it, and the real ceiling comes from the package's own metadata
    // (`applejackrp.sbproj`, `Metadata.MaxPlayers`). Passing it looks like it
    // works and does nothing, which is worse than not passing it.

    if let Some(gslt) = &config.gslt
        && !gslt.is_empty()
    {
        args.push("+net_game_server_token".to_owned());
        args.push(gslt.expose().to_owned());
    }

    if config.direct_connect {
        // Off by default: the default routes players through Steam's relay with
        // no inbound port, which is what the deployed server does today.
        args.push("+net_hide_address".to_owned());
        args.push("0".to_owned());
        args.push("+port".to_owned());
        args.push(config.port.to_string());
        args.push("+net_query_port".to_owned());
        args.push(config.query_port.to_string());
    }

    if bridge_needs_local_http {
        // Without this, `Http.IsAllowed` refuses direct IP literals, any host
        // resolving to a private or loopback address, and loopback on any port
        // other than 80/443/8080/8443. A bridge on a loopback or cluster-internal
        // address is exactly that, so the gamemode would refuse to reach it and
        // every write would sit owed in the journal with no obvious cause.
        args.push("-allowlocalhttp".to_owned());
    }

    args.extend(config.extra_args.iter().cloned());

    Command { program, args }
}

/// Where the engine writes its log file.
///
/// The derivation lives on [`ServerConfig`] because `validate()` needs it to
/// refuse two instances that would write to one file, and a second copy of it
/// here is a second copy that goes stale.
pub fn log_file_for(config: &ServerConfig) -> std::path::PathBuf {
    config.engine_log_file()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use cellar_core::config::ServerConfig;

    use super::*;

    fn config() -> ServerConfig {
        ServerConfig {
            executable: PathBuf::from("/home/container/sbox/sbox-server.exe"),
            project: PathBuf::from("/home/container/projects/applejackrp/applejackrp.sbproj"),
            game: None,
            map: None,
            launcher: Launcher::Wine,
            working_dir: None,
            log_file: None,
            hostname: "AppleJackRP Dev".to_owned(),
            gslt: None,
            direct_connect: false,
            port: 27015,
            query_port: 27016,
            ready_pattern: "Lobby created".to_owned(),
            extra_args: Vec::new(),
            data_dir: None,
        }
    }

    #[test]
    fn wine_runs_the_windows_binary_as_an_argument() {
        let command = command_for(&config(), false);
        assert_eq!(command.program, "wine");
        assert_eq!(command.args[0], "/home/container/sbox/sbox-server.exe");
    }

    #[test]
    fn native_runs_the_binary_directly() {
        let mut config = config();
        config.launcher = Launcher::Native;
        config.executable = PathBuf::from(r"C:\sbox\sbox-server.exe");

        let command = command_for(&config, false);
        assert_eq!(command.program, r"C:\sbox\sbox-server.exe");
        assert_eq!(command.args[0], "+game");
    }

    #[test]
    fn it_uses_game_and_never_project() {
        let command = command_for(&config(), false);
        assert!(command.args.iter().any(|a| a == "+game"));
        assert!(
            !command.args.iter().any(|a| a == "+project"),
            "+project idles at the bare console instead of booting a map"
        );
    }

    #[test]
    fn published_game_and_map_replace_the_local_project() {
        let mut config = config();
        config.game = Some("fobiat.applejackrp".into());
        config.map = Some("thieves.rpdowntown3t".into());

        let command = command_for(&config, false);
        let game = command.args.iter().position(|arg| arg == "+game").unwrap();
        assert_eq!(command.args[game + 1], "fobiat.applejackrp");
        assert_eq!(command.args[game + 2], "thieves.rpdowntown3t");
    }

    /// The correction: the deployed entrypoint passes `+maxplayers` today and
    /// the engine has no such switch, so it silently does nothing.
    #[test]
    fn it_never_passes_maxplayers() {
        let command = command_for(&config(), false);
        assert!(!command.args.iter().any(|a| a.contains("maxplayers")));
    }

    #[test]
    fn the_bridge_asks_for_allowlocalhttp() {
        assert!(
            command_for(&config(), true)
                .args
                .iter()
                .any(|a| a == "-allowlocalhttp")
        );
        assert!(
            !command_for(&config(), false)
                .args
                .iter()
                .any(|a| a == "-allowlocalhttp")
        );
    }

    #[test]
    fn direct_connect_is_off_unless_asked_for() {
        let command = command_for(&config(), false);
        assert!(!command.args.iter().any(|a| a == "+net_hide_address"));

        let mut config = config();
        config.direct_connect = true;
        let command = command_for(&config, false);

        let joined = command.args.join(" ");
        assert!(joined.contains("+net_hide_address 0"));
        assert!(joined.contains("+port 27015"));
        assert!(joined.contains("+net_query_port 27016"));
    }

    #[test]
    fn the_token_is_passed_but_never_printed() {
        let mut config = config();
        config.gslt = Some(Secret::new("ABCDEF0123456789TOKEN"));

        let command = command_for(&config, false);
        assert!(command.args.iter().any(|a| a == "ABCDEF0123456789TOKEN"));

        let printed = command.redacted(config.gslt.as_ref());
        assert!(!printed.contains("ABCDEF0123456789TOKEN"), "{printed}");
        assert!(printed.contains("+net_game_server_token ***"), "{printed}");
    }

    #[test]
    fn no_token_means_no_flag_rather_than_an_empty_one() {
        let command = command_for(&config(), false);
        assert!(!command.args.iter().any(|a| a == "+net_game_server_token"));
    }

    #[test]
    fn extra_args_are_appended_verbatim() {
        let mut config = config();
        config.extra_args = vec!["+sv_cheats".into(), "1".into()];
        let command = command_for(&config, false);
        assert_eq!(
            &command.args[command.args.len() - 2..],
            &["+sv_cheats", "1"]
        );
    }

    #[test]
    fn the_log_file_defaults_beside_the_executable() {
        assert_eq!(
            log_file_for(&config()),
            PathBuf::from("/home/container/sbox/logs/sbox-server.log")
        );

        let mut config = config();
        config.log_file = Some(PathBuf::from("/var/log/sbox.log"));
        assert_eq!(log_file_for(&config), PathBuf::from("/var/log/sbox.log"));
    }
}

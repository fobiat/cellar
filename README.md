# Cellar

Cellar supervises dedicated [s&box](https://sbox.game) servers. It provides a
CLI, a terminal dashboard, a responsive web control panel, health probes,
restart policy, and an audited operator console.

Cellar is gamemode-neutral. A profile supplies the readiness line, map list,
optional checks, and safe command palette entries. When a profile declares a
command prefix, Cellar also asks the running engine for `find <prefix>` and
shows valid commands automatically. Discovered commands require confirmation.
This keeps gamemode-specific behaviour in the gamemode profile instead of in
Cellar's core.

```sh
cellar doctor     # check the executable, profile, paths, and dependencies
cellar run        # supervise the configured server in the foreground
cellar status     # inspect a running Cellar instance
```

## Documentation

| | |
| --- | --- |
| **[Quickstart](docs/QUICKSTART.md)** | Start a local server and open the dashboard. |
| **[Installation](docs/INSTALLATION.md)** | Installers, Linux, Windows, containers, and services. |
| **[Configuration](docs/CONFIGURATION.md)** | Every `cellar.toml` key and profile field. |
| **[CLI reference](docs/CLI.md)** | Commands, flags, and output contracts. |
| **[Architecture](docs/ARCHITECTURE.md)** | Process supervision, the pty, profiles, and the bridge. |
| **[Operations](docs/OPERATIONS.md)** | Probes, updates, backups, notifications, and recovery. |
| **[Game database](docs/GAME_DATABASE.md)** | The gamemode-owned database contract. |
| **[MCP integration](docs/MCP.md)** | Read-only tools and authenticated command access. |
| **[Troubleshooting](docs/TROUBLESHOOTING.md)** | Common failures and evidence to collect. |
| **[Facepunch Sandbox](docs/FACEPUNCH-SANDBOX.md)** | The shipped generic profile example. |
| **[AppleJackRP integration](docs/integrations/applejackrp.md)** | Optional profile and workflow notes for AppleJackRP. |

The [docs/README.md](docs/README.md) explains the documentation layout. The
AppleJackRP page is an integration guide, not a Cellar dependency.

## What Cellar does

- Supervises a native or Wine-launched server in a pseudo-terminal, because
  the dedicated console is only created when the child has a real terminal.
- Tracks readiness from the configured profile instead of guessing from an open
  port.
- Exposes the same state to the CLI, TUI, web UI, probes, and WebSocket clients.
- Discovers and runs gamemode commands through the profile prefix without
  hardcoding a named gamemode.
- Connects to a database supplied by the gamemode. Cellar can inspect it, but
  it does not own game tables or silently run game migrations.
- Provides authenticated, audited operator actions and bounded read-only
  database queries.
- Supports health checks, graceful shutdown, crash-loop detection, backups,
  update checks, and webhook notifications.

## Install

Public releases need no token.

### Linux

```sh
version=v0.3.0-beta.1
curl -fsSLO "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.sh"
less install.sh
sh install.sh --version "$version"
rm install.sh
```

### Windows

```powershell
$version = 'v0.3.0-beta.1'
Invoke-WebRequest "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.ps1" -OutFile install-cellar.ps1
Get-Content .\install-cellar.ps1
.\install-cellar.ps1 -Version $version
Remove-Item .\install-cellar.ps1
```

Both installers verify the published checksum and install per-user by
default. Docker, Kubernetes, services, source builds, and uninstall steps are
covered by [Installation](docs/INSTALLATION.md).

## Minimal configuration

Start from [`cellar.toml.example`](cellar.toml.example). A profile is optional,
but a server should declare the readiness line it actually logs.

```toml
[server]
executable = "/srv/sbox/sbox-server"
game = "facepunch.sandbox"
map = "facepunch.flatgrass"
launcher = "native"

[profile]
name = "Facepunch Sandbox"
ready_pattern = "Connected to Steam"
convar_prefix = "sbox"

[[profile.command]]
label = "List players"
command = "status"
```

For local development, set `server.project` to the `.sbproj` instead of
`server.game`. On Linux, use `launcher = "wine"` only when the installed
dedicated server is a Windows executable and the Wine runtime is ready. The
native path is the default for a native server binary.

## Development

The repository gate is:

```sh
cargo fmt --all
cargo build --workspace --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

The fake server in `crates/cellar-fake-server` exercises supervision and
console behaviour without Steam, Wine, or an s&box installation. The complete
release checks also validate documentation, examples, generated files, and
the browser bundle.

## Boundary

Cellar owns process management and operator tooling. The game owns gameplay,
its source tree, its package publication, its schema, and its game-specific
policy. Integration-specific material belongs under
[docs/integrations/](docs/integrations/), so a new gamemode can use Cellar
without inheriting another game's assumptions.

## License

See [LICENSE](LICENSE).

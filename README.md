<p align="center">
  <img src="crates/cellar-server/src/ui/assets/cellar-logo-horizontal.svg" alt="Cellar">
</p>

# Cellar

Cellar supervises dedicated [s&box](https://sbox.game) servers. It provides a
CLI, a terminal dashboard, a responsive web control panel, health probes,
restart policy, and an audited operator console.

It turns an s&box process into an operated service: the real console, gamemode
readiness, command discovery, database workflows, recovery controls, and live
mobile-friendly views share one source of truth. See [Why Cellar?](docs/WHY-CELLAR.md)
for the comparison with running s&box directly or using a general-purpose
process manager.

Cellar is gamemode-neutral. A profile supplies the readiness line, map list,
optional checks, and safe command palette entries. When a profile declares a
command prefix, Cellar also asks the running engine for `find <prefix>` and
shows valid commands automatically. Discovered commands require confirmation.
This keeps gamemode-specific behaviour in the gamemode profile instead of in
Cellar's core.

```sh
cellar doctor     # check the executable, profile, paths, and dependencies
cellar run        # supervise the configured server in the foreground
cellar tui        # open a dashboard for an already running Cellar
cellar status     # inspect a running Cellar instance
```

## Documentation

| | |
| --- | --- |
| **[Quickstart](docs/QUICKSTART.md)** | Start a local server and open the dashboard. |
| **[Why Cellar?](docs/WHY-CELLAR.md)** | The practical advantages over direct s&box hosting and general-purpose managers. |
| **[Installation](docs/INSTALLATION.md)** | Installers, Linux, Windows, containers, and services. |
| **[Configuration](docs/CONFIGURATION.md)** | Every `cellar.toml` key and profile field. |
| **[CLI reference](docs/CLI.md)** | Commands, flags, and output contracts. |
| **[Architecture](docs/ARCHITECTURE.md)** | Process supervision, the pty, profiles, and the bridge. |
| **[Operations](docs/OPERATIONS.md)** | Probes, updates, backups, notifications, and recovery. |
| **[Branding](docs/BRANDING.md)** | Cellar logo assets and the shared AppleJack Framework visual language. |
| **[Game database](docs/GAME_DATABASE.md)** | The gamemode-owned database contract. |
| **[MCP integration](docs/MCP.md)** | Read-only tools and authenticated command access. |
| **[Security and privacy](docs/SECURITY.md)** | Threat model, credentials, telemetry, and reporting. |
| **[Troubleshooting](docs/TROUBLESHOOTING.md)** | Common failures and evidence to collect. |
| **[Facepunch Sandbox](docs/FACEPUNCH-SANDBOX.md)** | The shipped generic profile example. |
| **[AppleJack Framework integration](docs/integrations/applejack-framework.md)** | Optional profile and workflow notes for AppleJack Framework. |

The repository also carries a [fully populated AppleJack Framework Cellar
config](configs/applejack-framework.toml) for reference. It defaults to native
Linux, keeps secrets outside TOML, and shows persistence plus verified backup
copies.

The [published Cellar manual](https://fobiat.dev/applejack/docs/cellar/) is
available on the AppleJack Framework documentation site for browsing and
mobile use.

The [docs/README.md](docs/README.md) explains the documentation layout. The
AppleJack Framework page is an integration guide, not a Cellar dependency.

## What Cellar does

- Supervises a native or Wine-launched server in a pseudo-terminal, because
  the dedicated console is only created when the child has a real terminal.
- Tracks readiness from the configured profile instead of guessing from an open
  port.
- Exposes the same state to the CLI, TUI, web UI, probes, and WebSocket clients.
- Discovers and runs gamemode commands through the profile prefix without
  hardcoding a named gamemode.
- Offers a responsive operator UI with the AppleJack Framework visual language,
  touch-sized controls,
  dark surfaces, live status chips, and narrow-screen layouts.
- Connects to a database supplied by the gamemode. Cellar can inspect it, but
  it does not own game tables or silently run game migrations.
- Can optionally snapshot the gamemode's bridge documents as verified JSON,
  export a second copy elsewhere, and restore a selected snapshot safely.
- Provides authenticated, audited operator actions and bounded read-only
  database queries.
- Supports health checks, graceful shutdown, crash-loop detection, backups,
  update checks, and webhook notifications.
- Includes optional Windows and Linux tray launchers with web UI, TUI, status,
  and server controls.

## Install

Public releases need no token.

### Linux

```sh
version=v0.3.0-beta.1
curl -fsSLO "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.sh"
less install.sh
sh install.sh --tray --version "$version"
rm install.sh
```

### Windows

```powershell
$version = 'v0.3.0-beta.1'
Invoke-WebRequest "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.ps1" -OutFile install-cellar.ps1
Get-Content .\install-cellar.ps1
.\install-cellar.ps1 -Version $version -Tray
Remove-Item .\install-cellar.ps1
```

Both installers verify the published checksum and install per-user by
default. Docker, Kubernetes, services, source builds, and uninstall steps are
covered by [Installation](docs/INSTALLATION.md).

The tray is optional. Linux uses `yad` and a desktop autostart entry. Windows
uses a Startup shortcut and PowerShell's notification area support. Set
`CELLAR_SESSION` in the tray process environment when the web UI requires a
password. The tray never stores the password or session in the repository.

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
`server.game`. Native is the default on Linux and Windows. Set
`launcher = "wine"` explicitly only when the installed dedicated server is a
Windows binary and the Wine runtime is ready.

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

Persistence snapshots are the boundary between those responsibilities. Cellar
preserves bridge document keys and JSON bodies, while the gamemode remains the
authority for their meaning. AppleJack-specific setup lives in its integration
guide and is not required by Cellar.

## License

See [LICENSE](LICENSE).

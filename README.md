# Cellar

The open-source dedicated server manager for [s&box](https://sbox.game). One
binary supervises the game process, gives operators a console they can reach,
and serves a responsive web control panel that works on desktop and phone.

Runs on **Linux under Wine** (the container and Kubernetes case) and **Windows
natively**. Same `cellar.toml` either way.

```sh
cellar doctor     # check everything that would otherwise fail at startup
cellar run        # supervise, in the foreground
```

## Documentation

| | |
| --- | --- |
| **[Quickstart](docs/QUICKSTART.md)** | A server running in about ten minutes. Start here. |
| **[Installation](docs/INSTALLATION.md)** | Installers, Docker, Kubernetes, source, upgrading, uninstalling. |
| **[Configuration](docs/CONFIGURATION.md)** | Every `cellar.toml` key and its default. |
| **[CLI reference](docs/CLI.md)** | Every command and flag. |
| **[Game database](docs/GAME_DATABASE.md)** | Hosted database connection, read-only schema browser, and ownership contract. |
| **[Operations](docs/OPERATIONS.md)** | Probes, graceful shutdown, webhooks, updates, backups. |
| **[Troubleshooting](docs/TROUBLESHOOTING.md)** | Symptoms and fixes, including what is still unproven. |
| **[Architecture](docs/ARCHITECTURE.md)** | How it is built, and the engine findings behind it. |

---

## Why this exists

Two gaps, both of which the projects themselves had already written down.

**There was no way to ask the server anything.** The Kubernetes manifest says so:

> No readiness/liveness probes: this isn't HTTP … Revisit once there's a
> confirmed way to ask this server "are you actually serving."

**The game owns its persistence.** Cellar connects to the database supplied by
the gamemode, discovers its live schema, and exposes a read-only browser for
operators. It does not create game tables, run game migrations, or pretend to
understand domain data it was not given.

---

## The one thing worth knowing: it needs a terminal, not a pipe

The obvious design is "own the child's stdin and stdout and type commands into
stdin". That does not work, and the engine source says why
(`engine/Launcher/SboxServer/Launcher.cs`):

```csharp
// Redirecting console output (docker/wine/etc..) won't support this overlay, so don't bother with it
if ( !Console.IsOutputRedirected )
{
    console = new DedicatedServerConsole();
}
```

With a pipe, the console object is never built, so **nothing reads stdin at
all**. And when it is built, input arrives through `Console.ReadKey()`, which
throws on a redirected stream.

A pseudo-terminal satisfies both checks, because a pty *is* a terminal. Cellar
spawns the server on one. That is also why the existing Kubernetes deployment
needs `stdin: true, tty: true`, and why `docker run` needs `-it`.

What that buys: `DedicatedServerConsole.OnInput` dispatches through
`ConVarSystem.Run` with `allowProtected: true`, which is **full privilege**.
Native commands (`quit`, `kick`, `status`) and the gamemode's own console
commands are available with **zero changes to the gamemode**. Gamemode C# itself cannot do this;
`ConsoleSystem.Run` refuses anything protected.

There is no RCON in s&box. This is the substitute, and it is a better one.

---

## What it does

- **Supervises** the process: spawn under Wine or natively, restart with
  exponential backoff, crash-loop detection, and a graceful stop that sends
  `quit` and waits, because the engine installs no SIGTERM handler anywhere and
  a kill skips the Steam logoff and the convar save.
- **Reads two channels**: the pty for commands, their replies and the status
  bar's frame timings; `logs/sbox-server.log` for every event. Only one produces
  events, or every line would be counted twice.
- **Connects to hosted game databases**: the web UI introspects the live schema,
  browses tables, and runs capped read-only queries without imposing a schema.
- **Answers probes**: `/healthz` for liveness, `/readyz` for "is it actually
  serving", derived from the readiness line in the log rather than from a port
  being open.
- **Three interfaces**: a CLI, a ratatui dashboard, and a web UI, all reading one
  snapshot so they cannot disagree.
- **Works from a phone**: the dashboard is responsive, installable, reactive to
  WebSocket state, and can send browser alerts for server events.
- **Supports hosted databases**: connects to the MySQL or MariaDB instance
  supplied by the gamemode, introspects its live schema, and leaves migrations
  and game tables under gamemode control. An optional local MariaDB supervisor
  remains available for installations that need one.
- **Captures configuration as a file**: the 41 features, the 7 catalogued
  settings and the engine's convars, dumped to TOML or YAML, diffed and applied.
- **Checks versions and updates**: the gamemode's build stamp, the git checkout
  against its remote, and the Steam build id of the dedicated server, with a
  hands-off updater that **never restarts a populated server**.
- **Sends webhooks**: batched Discord embeds in the Cellar palette.

Full command list in the [CLI reference](docs/CLI.md).

---

## Install

The repository is **private**, so every download needs a token. `gh auth token`
prints one, and both installers and `cellar self-update` read it from
`CELLAR_GITHUB_TOKEN`. Without it GitHub answers 404 and the tools say so rather
than reporting "no releases".

**Windows**

```powershell
$env:CELLAR_GITHUB_TOKEN = gh auth token
irm https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.ps1 | iex
```

**Linux**

```sh
export CELLAR_GITHUB_TOKEN="$(gh auth token)"
curl -fsSL https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.sh | sh
```

Both are per-user by default and need no administrator. Both verify the
published SHA-256 before installing anything, and both refuse when no checksum
was published. System-wide installs, service registration, Docker, Kubernetes
and building from source are all in [Installation](docs/INSTALLATION.md).

For an image or service, installation can be followed by an automatic
configuration check and foreground run. The container image uses this flow by
default:

```sh
docker run --rm -it \
  -v "$PWD/cellar.toml:/etc/cellar/cellar.toml" \
  -v /path/to/sbox:/home/container/sbox \
  -p 8081:8081 \
  ghcr.io/fobiat/cellar:latest run
```

The entrypoint creates `/etc/cellar/cellar.toml` from the bundled example when
needed, runs `cellar doctor`, then starts `cellar run`. Pass `doctor`, `config`,
or another Cellar command to use the same image for that command.

### Hosted database and web authentication

Cellar treats the database as a gamemode-owned contract. Set the connection
outside the config file and keep migrations disabled unless Cellar is the
explicit schema owner:

```powershell
$env:CELLAR_DATABASE_URL = 'mysql://readonly_user:secret@db.example/cellargame'
$env:CELLAR_WEB_PASSWORD_HASH = (cellar hash-password)
```

```toml
[database]
enabled = true
schema_owner = "gamemode"
migrate_on_start = false

[web]
enabled = true
bind = "0.0.0.0:8081"
auth = "password"
```

Use `auth = "auto"` for loopback development, or `auth = "none"` only on a
loopback bind. The dashboard exposes connection metadata, table discovery,
read-only queries, live controls, WebSocket state, and browser notifications.
See [Game database](docs/GAME_DATABASE.md) for the ownership and least-privilege
rules.

---

## Testing

`cellar-fake-server` is a stand-in for `sbox-server.exe` that emits the engine's
real log formats, renders a real status bar (using the same `cellar-core` code
Cellar parses with, so the two cannot drift), echoes commands the way
`ConsoleInput.OnEnter` does, implements `quit`, and can be told to crash, hang or
flood. Every supervisor, TUI and web test runs against it with no Steam, no Wine
and no s&box.

```sh
cargo test --workspace

# The database tests are skipped unless a server is pointed at:
docker run -d --rm --name cellar-test-db \
  -e MARIADB_ROOT_PASSWORD=cellartest -e MARIADB_DATABASE=cellar \
  -p 33061:3306 mariadb:11
export CELLAR_TEST_DATABASE_URL='mysql://root:cellartest@127.0.0.1:33061/cellar'
cargo test --workspace
```

The gate before any commit:

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets --workspace -- -D warnings
cargo fmt --all --check
```

Releases are cut locally after the same checks pass:

```sh
cargo build --release -p cellar-cli -p cellar-fake-server
./scripts/cross-build-windows.sh   # docker plus mingw, no host toolchain needed
./scripts/package-release.sh
gh release create vX.Y.Z --verify-tag --notes-file CHANGELOG.md dist/release/*
```

---

## Not yet proven

Written down rather than glossed.

**The pty console under Wine specifically.** `Console.IsOutputRedirected`
behaving as expected through Wine's console layer is the assumption the whole
command channel rests on. It has been proven on Linux against a native child,
and the Windows binary has been proven end to end under Wine supervising a
Windows child on ConPTY. It has **not** been run against `sbox-server.exe`
itself under Wine.

The full list, and what happens if that assumption fails, is in
[Troubleshooting](docs/TROUBLESHOOTING.md#what-has-not-been-proven).

## Corrections Cellar makes to the existing setup

Both from engine source:

- **`+maxplayers` does nothing.** There is no such convar or launch switch; the
  real ceiling comes from the project's `.sbproj` `Metadata.MaxPlayers`. The
  current `entrypoint.sh` passes it and it is inert.
- **Steam A2S reports zero players.** The server calls `SteamGameServer_Init`
  with a real query port, but nothing calls `BUpdateUserData`, which is what
  Steam derives the player count from. A2S is a liveness signal, not a roster.

And one the engine could fix: the console status bar renders its uptime hour
with `{TotalHours:n0}`, which rounds rather than truncates, so a server up 40
minutes displays `1:40:00`. Cellar inverts the error and reports the true
figure. See [Architecture](docs/ARCHITECTURE.md#the-status-bar-is-two-lines-and-its-clock-is-wrong).

---

## Licence

MIT or Apache-2.0, at your option.

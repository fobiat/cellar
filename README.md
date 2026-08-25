# Cellar

A dedicated server runner and manager for [s&box](https://sbox.game), built for
[AppleJackRP](https://fobiat.dev/applejack). One binary that supervises the game
process, gives it a console you can actually reach, and implements the MySQL
persistence bridge the gamemode already has a client for.

Runs on **Linux under Wine** (the container and Kubernetes case) and **Windows
natively**. Same `cellar.toml` either way.

---

## Why this exists

Two gaps, both of which the projects themselves had already written down.

**There was no way to ask the server anything.** The Kubernetes manifest says so:

> No readiness/liveness probes: this isn't HTTP … Revisit once there's a
> confirmed way to ask this server "are you actually serving."

**There was no persistence bridge.** `Documentation/Design/20_PERSISTENCE.md` §6.3
specifies an HTTP service at `/v1/doc/{key}`, and the gamemode's client half is
written, tested and shipped as `Code/Storage/HostedDocumentStore.cs`. Nothing
implemented the server half, so `StorageDirector` logged:

> `hosted: the call sites are still synchronous, so every read answers
> Unavailable and every write stays owed in the journal`

Cellar closes both. It supervises the process **and** it is the bridge.

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
needs `stdin: true, tty: true`.

What that buys: `DedicatedServerConsole.OnInput` dispatches through
`ConVarSystem.Run` with `allowProtected: true`, which is **full privilege**.
Native commands (`quit`, `kick`, `status`) and all 95 `applejack_*` commands,
with **zero changes to the gamemode**. Gamemode C# itself cannot do this;
`ConsoleSystem.Run` refuses anything protected.

---

## What it does

- **Supervises** the process: spawn under Wine or natively, restart with
  exponential backoff, crash-loop detection, and a graceful stop that sends
  `quit` and waits, because the engine installs no SIGTERM handler anywhere and
  a kill skips the Steam logoff and the convar save.
- **Reads two channels**: the pty for commands and the status bar's frame
  timings, `logs/sbox-server.log` for every event. Only one produces events, or
  every line would be counted twice.
- **Is the bridge**: `GET`/`PUT`/`HEAD /v1/doc/{key}` over MySQL, to the exact
  contract `HostedDocumentProtocol.cs` expects, with a revision history so a bad
  write is recoverable.
- **Answers probes**: `/healthz` for liveness, `/readyz` for "is it actually
  serving", derived from the readiness line in the log rather than from a port
  being open.
- **Three interfaces**: a CLI, a ratatui dashboard, and a web UI, all reading one
  snapshot so they cannot disagree.
- **Browses the database**: a small read-only phpMyAdmin in the web UI, plus a
  document browser with revision history.
- **Checks versions and updates**: the gamemode's build stamp, the git checkout
  against its remote, and the Steam build id of the dedicated server, with a
  hands-off updater that **never restarts a populated server**.
- **Sends webhooks**: batched Discord embeds in the Applejack palette.

---

## Install

The repository is **private**, so every download needs a token. `gh auth token`
prints one, and both installers and `cellar self-update` read it from
`CELLAR_GITHUB_TOKEN`. Without it GitHub answers 404 and the tools say so
rather than reporting "no releases".

**Windows**

```powershell
$env:CELLAR_GITHUB_TOKEN = gh auth token
irm https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.ps1 -Headers @{Authorization="Bearer $env:CELLAR_GITHUB_TOKEN"} | iex
```

Per-user by default, into `%LOCALAPPDATA%\Programs\Cellar`, needing no
administrator. Add `-System -Service` from an elevated shell to install for all
users and register a Windows service.

**Linux**

```sh
export CELLAR_GITHUB_TOKEN="$(gh auth token)"
curl -fsSL -H "Authorization: Bearer $CELLAR_GITHUB_TOKEN" \
  https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.sh | sh
```

Per-user into `~/.local/bin`. Add `--system --service` as root for
`/usr/local/bin` plus a systemd unit whose `TimeoutStopSec` is long enough for
the engine's own shutdown.

Both verify the published SHA-256 before installing anything, and both refuse
when no checksum was published. `cellar self-update` does the same, and takes
the bare binary rather than the archive so replacing the running binary needs no
zip or tar reader.

If the repository is ever made public, every token above becomes unnecessary.

## Quick start

```sh
cargo build --release
cp cellar.toml.example cellar.toml    # then edit it
export CELLAR_DATABASE_URL='mysql://cellar:…@db/cellar'
./target/release/cellar doctor        # check before starting anything
./target/release/cellar run
```

`cellar doctor` checks the executable, the project, Wine, the database and the
log path, and tells you what is wrong rather than failing at startup.

### Commands

| | |
| --- | --- |
| `cellar run [--tui]` | Supervise in the foreground. The container entrypoint. |
| `cellar doctor` | Check everything that would otherwise fail at startup. |
| `cellar config` | Print the resolved config, secrets redacted. |
| `cellar version [--json]` | Installed and available versions. |
| `cellar changelog [--limit N]` | The gamemode's changelog. |
| `cellar update [--check\|--now\|--force]` | Check for updates, or take them. |
| `cellar settings dump\|diff\|apply\|set` | The running server's configuration, as a file. |
| `cellar db migrate\|status\|prune` | Schema and retention. |
| `cellar doc ls\|get\|put\|history` | Bridge documents from the shell. |
| `cellar hash-password` | An argon2 hash for the web UI. |

---

## Server configuration as a file

Today the only record of how a server is configured is the server. Cellar
captures the whole thing to TOML or YAML, so it can be committed, diffed and
applied somewhere else.

```sh
# What is this server set to?
cellar settings dump

# Just what somebody changed. This is the shape worth committing: a full dump
# is mostly defaults.
cellar settings dump --overrides -o server.toml

# What would this file change?
cellar settings diff server.toml

# Change it.
cellar settings apply server.toml
cellar settings set ui.menu.admin on
```

Three things share the screen because to an operator they are one question,
even though the engine keeps them apart: the 41 **features**, the 7 catalogued
**settings** with their bounds and documented sources, and the engine's own
**convars**.

Two behaviours worth knowing:

- **A partial file sets what it names and nothing else.** It is not a request to
  reset everything it omits, because that reading turns a three-line file into a
  way to wipe a server's configuration.
- **The live server's catalogue decides what is writable, not the file.** A file
  claiming a `core` feature is toggleable is still refused, and `boot` features
  are applied but flagged as needing a restart.

The same thing is clickable in the web UI's Ordinance tab, with an export
button for each format.

## The bridge

Cellar implements the contract the gamemode already speaks. Every status code
here is load-bearing:

| Method | Route | Answers |
| --- | --- | --- |
| `GET` | `/v1/doc/{key}` | `200` + JSON, `404` absent, anything else unavailable |
| `PUT` | `/v1/doc/{key}` | `204` written, anything else failed |
| `HEAD` | `/v1/doc/{key}` | `200` present, `404` absent |

**404 is the only code that may mean absent.** A 500, a 502 and a timeout are
all "I could not tell you". Mapping any of them to absent is §4.1's catastrophe
arriving over the wire: a player joins, their roster reads as empty, and the
empty roster is written back over their character. There is a test for it.

Two things Cellar sets up for you:

- **`hosting.json` is generated**, not hand-written, from the same config that
  decides what the bridge binds. `HostingConfigStore` refuses a malformed
  document loudly rather than falling back to local, and a hand-edited file is
  the way to trip that.
- **`-allowlocalhttp` is added to the launch line** when the bridge is enabled.
  Without it `Http.IsAllowed` blocks every private and loopback address, so the
  gamemode would silently refuse to reach a bridge on a cluster-internal
  address and every write would sit owed in the journal with no obvious cause.

One trap the spec calls out and Cellar cannot fix for you: the engine strips
`Authorization` on **every redirect hop**. Point `bridge.public_url` at the
final URL, never at something that redirects.

### Authentication, honestly

The gamemode authenticates with `Sandbox.Services.Auth.GetToken(audience)` as a
bearer token. That is a good scheme, because nothing secret ships inside the
game package.

**Cellar cannot verify those tokens.** Facepunch's service API publishes package,
version, stats, leaderboard, achievement, player, news, notification, storage,
utility and code endpoints, and no token introspection. There is no documented
way for a third party to check one.

So Cellar does not claim to:

- **`trusted`** (default) requires a well-formed bearer, records it, and does not
  verify it. The trust boundary is the process tree, not the token: only the game
  host ever talks to the bridge, and Cellar is the process that launched it. The
  config **refuses to start** `trusted` on a non-loopback bind, so that boundary
  stays real.
- **`shared_secret`** for a bridge that must be reachable off-box. Needs a small
  gamemode-side change to send the secret.
- **`facepunch`** is a documented, unimplemented seam. Selecting it is refused at
  startup rather than silently downgraded.

---

## Configuration

Everything is in `cellar.toml`. **Secrets never are.** They come from the
environment and are held in a type that prints as `***` in logs, in
`status --json`, and in `cellar config`.

| Variable | For |
| --- | --- |
| `CELLAR_DATABASE_URL` | MySQL connection string |
| `CELLAR_GSLT` | Steam Game Server Login Token |
| `CELLAR_BRIDGE_SECRET` | `shared_secret` auth |
| `CELLAR_WEB_PASSWORD_HASH` | Web UI operator password, from `cellar hash-password` |
| `CELLAR_DISCORD_WEBHOOK_URL` | Discord notifications |
| `CELLAR_WEBHOOK_URL` | Generic JSON notifications |

---

## The updater

Default policy is `notify`, not `apply`: taking an update is a decision you opt
into, not one you discover afterwards.

When it does apply, the gates are, in order: a dirty checkout defers (pulling
over uncommitted work is how an updater destroys it), **any connected player
defers**, and being outside the maintenance window defers. Only then does it
stop the server gracefully, update, and start it again. `git pull` is
`--ff-only`, because a merge commit created unattended at 4am is a merge nobody
reviewed.

```toml
[update]
policy = "apply"
only_when_empty = true      # leave this on
window_start_hour = 4       # 04:00 to 06:00 local
window_end_hour = 6
update_gamemode = true
update_engine = false       # needs steamcmd and steam_dir
```

---

## Kubernetes

`cellar run` replaces `server/entrypoint.sh`. The deployment gains what it asked
for in its own comment:

```yaml
stdin: true          # still needed: the engine wants a terminal
tty: true
terminationGracePeriodSeconds: 45   # at least supervisor.graceful_timeout_seconds
readinessProbe:
  httpGet: { path: /readyz, port: 8081 }
livenessProbe:
  httpGet: { path: /healthz, port: 8081 }
```

`/readyz` is 503 while starting, backing off or crash-looping, which is what
stops a rollout sending players at a server still loading its map. `/healthz`
deliberately does **not** check the game: a liveness probe that fails during a
legitimate restart gets the pod killed mid-restart.

---

## Testing

`cellar-fake-server` is a stand-in for `sbox-server.exe` that emits the engine's
real log formats, writes a status bar, reads console commands, implements
`quit`, and can be told to crash, hang or flood. Every supervisor, TUI and web
test runs against it, in CI, with no Steam, no Wine and no s&box.

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
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

---

## Not yet proven

Written down rather than glossed, because both are read from engine source
rather than observed on real hardware:

1. **The pty console under Wine specifically.** `Console.IsOutputRedirected`
   behaving as expected through Wine's console layer is the assumption the whole
   command channel rests on. It works on Linux against a native child; it has not
   yet been run against `sbox-server.exe` under Wine.
2. **The status bar's frame-timing layout.** The parser is deliberately tolerant
   and leaves timings `None` rather than refusing the line, so a wrong guess
   costs a missing number and not a broken log.

Two corrections Cellar makes to the existing setup, both from engine source:

- **`+maxplayers` does nothing.** There is no such convar or launch switch; the
  real ceiling comes from `applejackrp.sbproj`'s `Metadata.MaxPlayers`. The
  current `entrypoint.sh` passes it and it is inert.
- **Steam A2S reports zero players.** The server calls `SteamGameServer_Init`
  with a real query port, but nothing calls `BUpdateUserData`, which is what
  Steam derives the player count from. A2S is a liveness signal, not a roster.

---

## Licence

MIT or Apache-2.0, at your option.

# Quickstart

A supervised server with a web dashboard, in about ten minutes. No database and
no persistence yet: [step 6](#6-optional-turn-on-persistence) adds those once the
basics work.

You need an s&box dedicated server already installed (`sbox-server.exe` and a
built `.sbproj`). Cellar supervises that; it does not install the game server.

---

## 1. Install Cellar

**Windows** (PowerShell, pinned bootstrap):

```powershell
$version = 'v0.1.12'
Invoke-WebRequest "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.ps1" -OutFile install-cellar.ps1
Get-Content .\install-cellar.ps1
.\install-cellar.ps1 -Version $version
Remove-Item .\install-cellar.ps1
```

**Linux** (pinned bootstrap):

```sh
version=v0.1.12
curl -fsSLO "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.sh"
less install.sh
sh install.sh --version "$version"
rm install.sh
```

No `gh`? Download the archive for your platform from the
[releases page](https://github.com/fobiat/cellar/releases), unpack it, and put
`cellar` somewhere on your `PATH`. Full detail, including building from source,
is in [Installation](INSTALLATION.md).

Check it:

```sh
cellar --version
```

---

## 2. Write a config

Cellar reads `cellar.toml` from the working directory unless you pass
`-c/--config`. The installers write a starting config for you. For a manual
setup, start from the shipped example, which is commented throughout:

```sh
curl -fsSLO https://raw.githubusercontent.com/fobiat/cellar/v0.1.12/cellar.toml.example
cp cellar.toml.example cellar.toml
```

The smallest config that does something useful:

```toml
[server]
executable = "/home/container/sbox/sbox-server.exe"
project    = "/home/container/projects/my-game/my-game.sbproj"
launcher   = "wine"          # "native" on Windows
hostname   = "S&box Server"

[database]
enabled = false              # turned on in step 6

[bridge]
enabled = false              # turned on in step 6

[web]
enabled = true
bind    = "127.0.0.1:8081"
```

On Windows, the same three server lines with Windows paths:

```toml
[server]
executable = 'C:\sbox\sbox-server.exe'
project    = 'C:\Projects\MyGame\my-game.sbproj'
launcher   = "native"
```

Every key is documented in [Configuration](CONFIGURATION.md).

---

## 3. Check it before starting anything

```sh
cellar doctor
```

`doctor` verifies the executable exists and is runnable, the project file parses,
Wine is present when `launcher = "wine"`, the log path is writable, and the
database is reachable when enabled. It reports what is wrong instead of letting
the server fail at startup with something less clear.

Fix anything it reports before continuing. [Troubleshooting](TROUBLESHOOTING.md)
covers the common ones.

---

## 4. Start the server

```sh
cellar run
```

`run` is the foreground supervising mode, and it is what a container entrypoint
should call. You should see the server start, then a readiness line once it is
serving.

Add the terminal dashboard if you have a real terminal:

```sh
cellar run --tui
```

Stop it with `Ctrl-C`. Cellar sends the engine a `quit` and waits up to
`graceful_timeout_seconds` before killing it, so the nine shutdown steps
(including the Steam logoff that removes the server from the master list)
actually run. This is a genuine fix, not a nicety: the engine installs no
SIGTERM handler at all, so anything that kills the process skips all of them.

---

## 5. Open the dashboard

With `cellar run` still going, visit <http://127.0.0.1:8081>.

You get the live console, the player roster, the feature and setting editors, a
read-only database browser, and version information. Tabs are themed as a city
RP dispatch board: Dispatch, Roster, Precinct, Ordinance, Records, Registry,
Database, Releases.

> **The web UI runs commands at full engine privilege.** On loopback that is
> fine. Binding it anywhere else **requires** a password, and Cellar refuses to
> start otherwise. See [step 7](#7-optional-expose-the-dashboard).

While the server runs, a second terminal can drive it:

```sh
cellar settings dump                      # everything it is set to
cellar settings dump --overrides -o server.toml   # just what was changed
cellar settings diff server.toml          # what would this file change?
cellar settings apply server.toml         # change it
cellar settings set ui.menu.admin on      # one thing
```

These reach the running server through its web API, so `web.enabled` must be
true. A partial file **sets what it names and nothing else**; it is not a request
to reset what it omits.

---

## 6. Optional: turn on persistence

Start the MySQL or MariaDB database supplied by the gamemode, then:

```sh
export CELLAR_DATABASE_URL='mysql://cellar:secret@127.0.0.1/cellar'
```

```toml
[database]
enabled = true
schema_owner = "gamemode"
migrate_on_start = false
```

Restart Cellar and open the Database tab. Cellar discovers the live tables and
keeps queries read-only. It does not create game tables or run gamemode
migrations. See [Game database](GAME_DATABASE.md) for the contract.

```sh
cellar db status
cellar run
```

---

## 7. Optional: expose the dashboard

Only do this behind something that terminates TLS. The console behind this page
runs arbitrary engine commands.

```sh
cellar hash-password                       # prompts, prints an argon2 hash
export CELLAR_WEB_PASSWORD_HASH='$argon2id$v=19$...'
```

```toml
[web]
enabled = true
bind    = "0.0.0.0:8081"
allow_insecure_http = true
secure_cookies = true
```

Cellar refuses to start on a non-loopback address without
`CELLAR_WEB_PASSWORD_HASH`. That is deliberate and not overridable.

---

## Where to go next

- Running it in Kubernetes, with probes and a grace period: [Operations](OPERATIONS.md)
- Every config key: [Configuration](CONFIGURATION.md)
- Every command: [CLI reference](CLI.md)
- Something is wrong: [Troubleshooting](TROUBLESHOOTING.md)

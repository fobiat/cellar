# Configuration

Cellar reads `cellar.toml` from the working directory unless you pass
`-c/--config`. [`cellar.toml.example`](../cellar.toml.example) at the repo root
is a commented starting point.

`cellar config` prints what the file, the environment and the defaults resolve
to together, with secrets redacted. That output is what actually takes effect,
and it is safe to paste into an issue.

---

## Secrets come from the environment, never the file

No secret is ever read from `cellar.toml`, and none is ever written back to it.

| Variable | Used for |
| --- | --- |
| `CELLAR_DATABASE_URL` | `mysql://user:password@host/database`. Takes precedence over `database.url_file`. |
| `CELLAR_GSLT` | Steam Game Server Login Token. |
| `CELLAR_BRIDGE_SECRET` | Required when `bridge.auth = "shared_secret"`. |
| `CELLAR_WEB_PASSWORD_HASH` | From `cellar hash-password`. Required for a non-loopback `web.bind`. |
| `CELLAR_DISCORD_WEBHOOK_URL` | Discord notifications. |
| `CELLAR_WEBHOOK_URL` | Generic JSON notifications. |
| `CELLAR_WEB_PASSWORD` | The plain password, so the CLI's live-server commands can log in. |
| `CELLAR_API_TOKEN` | Bearer token for the read-only external API under `/api/v1`. |
| `CELLAR_GITHUB_TOKEN` / `GITHUB_TOKEN` | Reading releases from a private fork. |
| `CELLAR_LOG` | Log filter. Same as `--log`. |

Secrets are held in a type that prints as `***` in every debug, display and
serialisation path, so a config dump or a crash log cannot leak one.

---

## `[server]`

| Key | Default | Meaning |
| --- | --- | --- |
| `executable` | *required* | Path to `sbox-server.exe`. |
| `project` | *required unless `game` is set* | Path to the local `.sbproj`. |
| `game` | unset | Published package ident such as `fobiat.applejackrp`. |
| `map` | unset | Map ident appended to a published game ident, such as `thieves.rpdowntown3t`. |
| `launcher` | `wine` | `wine` or `native`. `native` on Windows. |
| `wine_prefix` | inherited | The prefix this instance runs in, passed as `WINEPREFIX`. Concurrent instances on Linux need one each. |
| `hostname` | `"S&box Server"` | Server name, and what the status bar shows. |
| `working_dir` | inherited | Working directory for the child. It does **not** decide where the engine writes; see below. |
| `data_dir` | unset | The engine's data directory. **Required when the bridge is on**: Cellar writes `hosting.json` here. |
| `log_file` | `<executable dir>/logs/sbox-server.log` | Where Cellar **reads** the engine's log from. |
| `direct_connect` | `false` | Expose the real address. Off means players route through Steam's relay. |
| `port` | `27015` | Game port. Only meaningful with `direct_connect`. |
| `query_port` | `27016` | Steam query port. |
| `gslt` | from env | Steam Game Server Login Token. |
| `ready_pattern` | from `[profile]` | The log line that means "serving". Drives `/readyz`. Overrides the gamemode profile for this instance only. |
| `extra_args` | `[]` | Extra arguments appended to the launch line. |

Two deliberate omissions:

- **No `maxplayers`.** The engine has no such convar and no such launch switch.
  The real ceiling is `Metadata.MaxPlayers` in the `.sbproj`. Setting
  `+maxplayers` does nothing. AppleJackRP's Cellar config leaves it out.
- **`-allowlocalhttp` is added automatically** when the bridge is enabled, and
  only then. Without it the engine refuses to call any private or loopback
  address and the gamemode silently cannot reach the bridge.

If `data_dir` is unset while the bridge is on, the gamemode never learns about
the bridge and quietly keeps writing to local files. `cellar doctor` says so.

**`log_file` is a read path, not a write path.** The engine writes
`logs/sbox-server.log` beside its own executable and nothing Cellar passes moves
it: not `working_dir`, and not `FACEPUNCH_ENGINE`, which was measured on
2026-09-01 and moves neither the log nor the data directory. Point `log_file` at
a file the engine does not write and readiness silently never fires, because the
tailer is watching something nothing writes to.

---

## `[instances.<id>]`

The other spelling of `[server]`, for running more than one server in one Cellar
process. A config uses one or the other, never both.

```toml
[instances.published]
scope = "applejackrp-local"

[instances.published.server]
executable = "/srv/published/sbox-server.exe"
game = "fobiat.applejackrp"
data_dir = "/srv/published/data/fobiat/applejackrp"

[instances.dev]
enabled = false

[instances.dev.server]
executable = "/srv/dev/sbox-server.exe"
project = "/srv/dev/applejackrp.sbproj"
data_dir = "/srv/dev/data/fobiat/applejackrp#local"
```

| Key | Default | Meaning |
| --- | --- | --- |
| `scope` | the id | Whose data this is: the `scope` column in `aj_document` and the operations tables. |
| `enabled` | `true` | Declared but not started. How a Windows-only development instance stays in a file that also deploys to Linux. |
| `required` | `true` | Whether `/readyz` speaks for this instance. |
| `server` | required | Exactly the `[server]` table, nested. |
| `supervisor` | inherits `[supervisor]` | Per-instance override. |
| `bridge` | inherits `[bridge]` | Per-instance override. |
| `profile` | inherits `[profile]` | Per-instance override, for the rare config whose instances run different gamemodes. |

The id must match `[a-z0-9][a-z0-9-]{0,31}`. It ends up in a URL query value, a
metrics label, a tracing span and a database column, and restricting it once at
parse time is what makes it safe in all four.

**The id and the scope are different things on purpose.** The id is the routing
key, the scope is the storage key. A deployment gaining a second instance names
its existing scope explicitly so its documents stay where they are.

**A `[server]` config keeps the scope it has today.** It desugars to one
instance with the id `default` and the scope taken from the global
`bridge.scope`, not from the id, so nothing moves and no migration runs.

**Each instance needs its own s&box install directory.** Both `logs/` and
`data/` follow the executable's own directory, measured rather than assumed, and
neither `server.working_dir` nor `FACEPUNCH_ENGINE` moves them. See
[Architecture](ARCHITECTURE.md#where-the-engine-writes-two-instances-cannot-share-an-install-tree).
`validate()` refuses two enabled instances that would collide.

**Addressing one instance over HTTP:** `?instance=<id>` on the existing routes,
defaulting to the primary, which is the first enabled instance in id order. It
is accepted by everything about one supervised server: status, exec, control,
logs, access, the settings catalogue, resources and documents. The dashboard
sends it only when a config declares more than one instance, so a single-server
deployment's URLs and access log are exactly as they were.
`GET /api/instances` lists what a config declares. An unknown id is a 404 naming
the ids that do exist, never a silent fallback, because the request that would
get misrouted that way is `quit`. `/api/control/exit` is process-wide and
deliberately ignores the parameter: exiting with another instance still running
gets that one killed rather than stopped.

**On Linux each also needs its own `server.wine_prefix`.** Every Wine process in
a prefix shares one `wineserver` that jointly holds all their sockets, and
`wineserver -k` is prefix-scoped, so with one prefix neither instance can be
dealt with without reaching the other. A prefix needs the Windows .NET 10
runtime installed into it; `cellar doctor` checks for it.

---

## `[profile]`

What gamemode this Cellar is running, described by the gamemode rather than
assumed by Cellar. Four things used to be hardcoded to AppleJackRP: the
readiness line, the log category heuristic, a `cellar doctor` check that grepped
one C# file by path, and thirteen command buttons in the web UI. All four come
from here now.

```toml
profile_file = "profiles/applejackrp.toml"
```

or inline:

```toml
[profile]
name = "AppleJackRP"
ready_pattern = "Lobby created - session is joinable"
convar_prefix = "applejack"

[[profile.command]]
group = "features"
label = "List features"
command = "applejack_features"

[[profile.check]]
name = "spawn validation"
file = "Code/Characters/CharacterDirector.cs"
contains = ["GroundedOrAuthored", "Scene.Trace"]
reason = "players spawn inside geometry without it"
```

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | unset | Shown in the UI. Nothing routes on it. |
| `ready_pattern` | AppleJackRP's line | The log line that means "serving". A substring match, not a regex. |
| `convar_prefix` | unset | The prefix this gamemode's convars share. Drives the log category filter. |
| `command` | `[]` | Array of tables: `label`, `command`, optional `group`, optional `confirm`. Becomes the Precinct palette. |
| `check` | `[]` | Array of tables: `name`, `file`, `contains`, `reason`. Becomes `cellar doctor` checks named `gamemode: <name>`. |

`profile_file` is read relative to the config file's own directory, and a config
may set it or `[profile]`, not both. Six shipped AppleJackRP configs point at
one file rather than each carrying a copy, and a gamemode is welcome to ship a
profile next to its `.sbproj`.

**The readiness line is the field that matters.** `facepunch.sandbox` never logs
AppleJackRP's line, so before profiles existed it sat at `starting` and returned
503 from `/readyz` forever against a server that was bound, Steam-connected and
answering A2S. In Kubernetes that configuration never passes readiness and the
pod is killed and retried. `configs/profiles/facepunch-sandbox.toml` records
what that gamemode actually logs, measured rather than guessed.

**A check path is relative and may not climb out of the project directory.** A
config file is not a licence to read arbitrary host files back through `cellar
doctor`'s output; `validate()` refuses an absolute path or a `..` component.

**What this must not become:** an install script, a config-rewrite language, or
per-gamemode UI layout. A profile describes a gamemode. It does not configure
one.

---

## `[supervisor]`

| Key | Default | Meaning |
| --- | --- | --- |
| `restart` | `on_failure` | `never`, `always`, or `on_failure`. |
| `graceful_timeout_seconds` | `30` | How long a `quit` gets before the process is killed. |
| `sample_interval_seconds` | `2` | How often to sample CPU and memory. |
| `start_timeout_seconds` | `600` | How long a server may sit in `starting` before Cellar calls it `unhealthy`. `0` disables the check. |

`on_failure` restarts only when the exit was neither asked for nor clean.

**`start_timeout_seconds` never kills anything.** A server that never becomes
ready is otherwise indistinguishable from one still starting, and both observed
causes are permanent: a `server.ready_pattern` the gamemode never emits, and an
engine that fails to resolve a package and then idles at the console instead of
exiting. When the deadline passes the state becomes `unhealthy`, which is not
ready, so a readiness probe still refuses it, and the process is left alone. The
first cause is a healthy server with a wrong pattern in front of it, and killing
that would be the wrong answer. The default is generous on purpose: a cold
`facepunch.sandbox` start measured about 120 seconds, and a first run also
compiles packages.

**`graceful_timeout_seconds` is not decoration.** The engine installs no SIGTERM
handler, so anything that kills the process skips all nine shutdown steps,
including `ConVarSystem.SaveAll()` and the Steam `LogOff()` that removes the
server from the master list. Under Kubernetes, `terminationGracePeriodSeconds`
must be at least this large or the kubelet kills the pod mid-shutdown.

### `[supervisor.backoff]`

| Key | Default | Meaning |
| --- | --- | --- |
| `initial` | `2` | Seconds before the first restart. |
| `maximum` | `120` | Ceiling on the delay. |
| `multiplier` | `2.0` | Growth per consecutive failure. |
| `crash_loop_threshold` | `5` | Failures within `window` before giving up. |
| `window` | `300` | Seconds the threshold is counted over. |
| `healthy_after` | `60` | Seconds of uptime that resets the backoff. |

The window matters more than the count: a server that restarted twenty times
over a month is fine, and one that restarted five times in a minute is not.

---

## `[database]`

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Needs `CELLAR_DATABASE_URL` or `url_file`. |
| `url_file` | *(none)* | Protected one-line MySQL/MariaDB URL file for an externally hosted source. Environment URL wins when both are set. |
| `max_connections` | `8` | Connection pool ceiling. |
| `schema_owner` | `gamemode` | `gamemode` means Cellar only inspects the live schema. `cellar` retains the legacy operational migrations. |
| `migrate_on_start` | `false` | Legacy opt-in. Ignored when `schema_owner = "gamemode"`. |
| `event_retention_days` | `90` | What `cellar db prune` deletes. |

The recommended setup is a hosted game database owned by the gamemode. See
[Game database](GAME_DATABASE.md). The browser is read-only and should use a
database account with a matching `SELECT` grant.

For a published game that does not use AppleJackRP's database contract, see
the [Facepunch Sandbox profile](FACEPUNCH-SANDBOX.md). It leaves the database
and bridge disabled while retaining Cellar's process supervision and dashboard.

MariaDB and MySQL are both supported and they are not identical; the differences
that bite are recorded in [Troubleshooting](TROUBLESHOOTING.md#mariadb-and-mysql-differ).

---

## `[mariadb]`

Cellar hosting its own MariaDB, rather than only connecting to one running
elsewhere. Independent of `[database]`: this section only decides whether
`cellar run` also downloads (if needed), initializes (if needed) and
supervises a `mariadbd` process. `database.url`/`CELLAR_DATABASE_URL` is
still the one source of the connection string either way; `cellar mariadb
provision` prints one for you to set, it does not write one anywhere itself.

| Key | Default | Meaning |
| --- | --- | --- |
| `managed` | `false` | Download, initialize and supervise a local instance. |
| `version` | *(none)* | Exact version, e.g. `"11.4.5"`. Never `"latest"`. |
| `sha256` | *(none)* | sha256 of the official win64 archive for `version`, checked once by hand against MariaDB's published hashes at [mariadb.org/download](https://mariadb.org/download/). Required when `managed`. |
| `install_dir` | *(none)* | Where the binaries are unpacked. Required when `managed`. |
| `data_dir` | *(none)* | Where the data directory lives, independent of `install_dir` so a version upgrade never touches it. Required when `managed`. |
| `port` | `33306` | Loopback port. Not 3306, so a stray existing MySQL/MariaDB install on this machine is never silently collided with. |
| `database` | `cellar` | Created during provisioning. Must match `[A-Za-z_][A-Za-z0-9_]*`. |
| `username` | `cellar` | Same charset restriction; both are interpolated into the bootstrap SQL. |
| `restart` / `backoff` | same shape as `[supervisor]` | The restart policy for the `mariadbd` child. |
| `graceful_timeout_seconds` | `30` | How long a clean `mariadb-admin shutdown` is given before killing the process. |

There is deliberately no `bind` key. The listener is hardcoded to `127.0.0.1`
in the supervisor itself, not offered as a config choice: this server has no
legitimate reason to be reachable off this machine, and not exposing the
setting is a stronger guarantee than validating against a bad value.

**Why a bundled binary and not a container.** The obvious alternative is a
Docker/Podman container, and that is deliberately not what this does: see
[Architecture](ARCHITECTURE.md#hosting-mariadb-natively-not-in-a-container)
for why.

### Provisioning

```
cellar mariadb provision
```

Downloads the archive for `mariadb.version` (skipped if already installed),
initializes `mariadb.data_dir` (skipped if already initialized), then always
(re-)creates the database, user and a freshly generated password, printing:

```
CELLAR_DATABASE_URL='mysql://cellar:...@127.0.0.1:33306/cellar'
```

Set that in your environment before `cellar run`. The password is never
written to any file Cellar controls; if it is lost, running `provision` again
is how it is recovered, since the app user's grant is always reissued.
`cellar mariadb status` reports what is installed and provisioned without
starting anything. See [Installation](INSTALLATION.md#hosting-mariadb-locally)
for the full walkthrough.

---

## `[bridge]`

The half of AppleJackRP's storage contract that nothing implemented until now.
See [The bridge](BRIDGE.md) for the protocol.

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Serve `/v1/doc/{key}`. |
| `bind` | `127.0.0.1:8080` | Listen address. |
| `public_url` | `http://127.0.0.1:8080` | What goes in `hosting.json`. **Must be final, never a redirect.** |
| `auth_audience` | `applejack-bridge` | The `Auth.GetToken` service name. |
| `auth` | `trusted` | `trusted`, `shared_secret`, or `facepunch`. |
| `scope` | `default` | Which server's documents these are. |
| `max_body_bytes` | `1048576` | Request body ceiling. |
| `rate_limit_per_minute` | `600` | Per-caller rate limit. |

**`public_url` must not redirect.** The engine strips the `Authorization` header
on every redirect hop, so a redirecting URL turns every authenticated request
into an unauthenticated one.

### The three auth modes, honestly

- **`trusted`** (default) requires a well-formed bearer token, records it, and
  **does not verify it**. This is safe here because the trust boundary is the
  process tree, not the token: Cellar launched the game host, and the bridge
  binds loopback. Cellar refuses `trusted` on any non-loopback address.
- **`shared_secret`** verifies a configured token from `CELLAR_BRIDGE_SECRET`.
  It needs a gamemode-side change to send one, so it is opt-in.
- **`facepunch`** would verify a platform-issued token. **No published endpoint
  for doing that was found**, so this mode refuses to start rather than
  pretending. It is a documented seam, not a feature.

This is spelled out because "we verify the token" would be false, and a false
security claim is worse than an absent one.

---

## `[web]`

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Serve the dashboard and the control API. The shipped example turns this on. |
| `bind` | `127.0.0.1:8081` | Listen address. |
| `auth` | `auto` | `auto` uses a password when configured, `password` always requires one, and `none` is loopback-only. |
| `allow_insecure_http` | `false` | Required for a non-loopback bind behind a TLS-terminating reverse proxy. |
| `secure_cookies` | `false` | Required for a non-loopback bind so browser sessions are marked Secure. |
| `password_hash` | from env | Argon2 hash from `cellar hash-password`. |

`auth = "password"` requires `CELLAR_WEB_PASSWORD_HASH` even on loopback.
`auth = "none"` is refused on a non-loopback bind. With `auth = "auto"`, a
non-loopback bind still requires `CELLAR_WEB_PASSWORD_HASH`. The console behind
this page runs `ConVarSystem.Run` with `allowProtected: true`, which is full
engine privilege.

`web.enabled` must be true for `cellar settings` to work at all, because those
commands reach the running server through this API.

Cellar does not terminate TLS itself. A non-loopback UI bind must explicitly
set `allow_insecure_http = true` and `secure_cookies = true`, then sit behind a
TLS reverse proxy. This prevents an accidental plain-HTTP deployment from
silently carrying operator credentials.

---

## `[notify]`

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Send webhooks. |
| `batch_seconds` | `5` | Coalescing window, so a join wave is one message. |
| `kinds` | `[]` | Which event kinds to send. Empty means every notable kind. |

Valid `kinds` values:

`process_started`, `process_exited`, `server_ready`, `player_joined`,
`player_left`, `command_dispatched`, `command_replied`, `bridge_health`,
`status`, `resources`, `log`, `unparsed`

Batching is not optional politeness: Discord rate-limits bursts, and thirty
players joining at once is thirty messages without it.

Webhook URLs come from the environment, are never logged, and are redacted from
`cellar config`.

---

## `[update]`

| Key | Default | Meaning |
| --- | --- | --- |
| `policy` | `notify` | `off`, `notify`, or `apply`. |
| `check_interval_minutes` | `60` | How often to look. |
| `check_remote` | `true` | Consult the git remote as well as the local stamp. |
| `only_when_empty` | `true` | Never restart a populated server. |
| `window_start_hour` | `0` | Local-hour maintenance window start. |
| `window_end_hour` | `0` | Window end. Equal values mean any time; `22`–`4` wraps midnight. |
| `update_gamemode` | `true` | Update the gamemode checkout. |
| `update_engine` | `false` | Update the engine through SteamCMD. |
| `program_check` | `true` | Check the Cellar release feed in the background. Checks report only. |
| `program_check_interval_minutes` | `60` | Minutes between Cellar release checks, with a five-minute floor. |
| `program_release_url` | Cellar GitHub releases API | Release endpoint for the Cellar program check. |
| `steam_dir` | unset | Needed for `update_engine` and for reading the installed build id. |
| `steamcmd` | unset | Path to `steamcmd`. |

**Leave `only_when_empty` on.** An updater that restarts a server with people on
it is worse than no updater. `cellar update --force` is the deliberate override,
and it announces itself before acting.

---

## `[backup]`

Logical dumps of whatever `database.url` points at. See
[`cellar db restore`](CLI.md#cellar-db-restore-dump---yes) for the other half:
a backup nobody has restored is a hypothesis.

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Take a dump on a timer while `cellar run` is up. |
| `directory` | unset | Where dumps go. Falls back to `mariadb.data_dir/backups`. |
| `interval_hours` | `24` | Hours between dumps. |
| `retain` | `7` | How many to keep. Older ones are deleted after each dump. |

**Only one process may own this.** Two Cellar processes with `enabled = true`
and the same directory run two loops with the same `retain`, so each prunes the
other's dumps and neither keeps seven.

---

## `[release]`

Project-local commands for building and publishing the game, exposed on the
Releases tab. Cellar never invents an s&box editor command: the editor owns the
Steam session, so the exact commands come from the operator.

| Key | Default | Meaning |
| --- | --- | --- |
| `build_command` | empty | Argv for a build. Empty disables the button. |
| `publish_command` | empty | Argv for a publish. Empty disables the button. |
| `working_dir` | unset | Where to run them. Defaults to the project directory. |

---

## Validation

Cellar refuses to start rather than running in a state that will confuse you
later. It rejects:

- `database.enabled` without `CELLAR_DATABASE_URL`
- `bridge.auth = "shared_secret"` without `CELLAR_BRIDGE_SECRET`
- `bridge.auth = "facepunch"`, always, as unimplementable today
- `bridge.auth = "trusted"` on a non-loopback `bind`
- `web.enabled` on a non-loopback `bind` without `CELLAR_WEB_PASSWORD_HASH`
- a missing `executable` or `project`
- two enabled instances sharing a log file, a data directory, a document scope,
  a bridge address, or `server.port` under `direct_connect`. Not reachable
  today, since a config has one `[server]` table; it exists so the
  concurrent-instance work cannot introduce the collision

`cellar doctor` reports all of these before you start anything.

# Operations

## API surfaces

Cellar exposes two separate integration surfaces. The internal document bridge
is `/v1/doc/{key}` on `bridge.bind` and is the game server's storage contract.
The operator API is served on `web.bind`. Set `CELLAR_API_TOKEN` to enable its
read-only machine endpoints:

```text
GET /api/v1/status?instance=published
GET /api/v1/logs?q=storage&level=error&limit=500
GET /api/v1/resources
GET /api/v1/addresses
Authorization: Bearer <CELLAR_API_TOKEN>
```

`?instance=<id>` names which server a request is about, on both `/api/v1` and
the dashboard's own routes. Omitting it means the primary, so a single-server
deployment's existing calls are unchanged. An unknown id is a 404 listing the
ids that exist, never a silent fall back to the primary: the request that would
be misrouted that way is `quit`. `GET /api/instances` lists what a config
declares.

The API token is read from the environment and never written to TOML or
returned by status. Keep the web bind private or put it behind TLS and an
appropriate network policy.

Running Cellar for real: health, shutdown, Kubernetes, notifications, updates and
backups.

---

## Health endpoints

Served on **both** `bridge.bind` and `web.bind`, so a probe can use whichever
port is already exposed. If the bridge is disabled they are on `web.bind` only,
and vice versa.

| Route | Answers |
| --- | --- |
| `/healthz` | `200` while the process is up. Liveness. |
| `/readyz` | `200` once the server is actually serving, `503` before that. Readiness. |

`/readyz` is derived from a log line: `server.ready_pattern` if the instance
sets one, otherwise the gamemode's `[profile]`, otherwise AppleJackRP's
`Lobby created - session is joinable`. Its body says what it knows, for example
`running, 2 player(s)`.

**A readiness line the gamemode never logs is the failure to look for first.**
It is indistinguishable from a slow start: the server binds its ports, connects
to Steam and answers A2S while `/readyz` stays at 503 and Kubernetes kills the
pod. `GET /api/instances` reports each instance's resolved `ready_pattern`, so
the answer is one request rather than a guess. See
[Configuration](CONFIGURATION.md#profile).

This closes a gap the deployment manifest wrote down itself:

> No readiness/liveness probes: this isn't HTTP … Revisit once there's a
> confirmed way to ask this server "are you actually serving."

It is HTTP now.

Do **not** use Steam A2S for this. The server does answer `A2S_INFO`, but nothing
in the engine ever calls `BUpdateUserData`, so the player count and the
`A2S_PLAYER` list are always empty. A2S is a liveness and identity check only,
and only when `direct_connect` is on.

---

## Graceful shutdown

**The engine installs no SIGTERM handler.** No `CancelKeyPress`, no
`PosixSignalRegistration`, nothing. Killing the process skips all nine clean
shutdown steps, including `ConVarSystem.SaveAll()` and the Steam `LogOff()` that
removes the server from the master list.

Today, every Kubernetes rollout kills it exactly that way.

Cellar's stop sequence:

1. Write `quit` into the console.
2. Wait up to `supervisor.graceful_timeout_seconds` (default 30).
3. Kill if it has not exited.

So `terminationGracePeriodSeconds` must be **at least** the graceful timeout, or
the kubelet kills the pod partway through the shutdown Cellar just started and
you are back where you began.

---

## Kubernetes

```yaml
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      terminationGracePeriodSeconds: 45     # >= graceful_timeout_seconds
      containers:
        - name: server
          image: cellar:latest
          args: ["run", "-c", "/etc/cellar/cellar.toml"]

          # Without both of these the engine builds no console and every
          # command silently does nothing. This is the single most common way
          # to deploy a broken server that looks fine.
          stdin: true
          tty: true

          readinessProbe:
            httpGet: { path: /readyz, port: 8080 }
            initialDelaySeconds: 10
            periodSeconds: 5
          livenessProbe:
            httpGet: { path: /healthz, port: 8080 }
            periodSeconds: 10

          env:
            - name: CELLAR_DATABASE_URL
              valueFrom:
                secretKeyRef: { name: cellar, key: database-url }
            - name: CELLAR_WEB_PASSWORD_HASH
              valueFrom:
                secretKeyRef: { name: cellar, key: web-password-hash }
```

Points worth stating:

- **`stdin: true` and `tty: true` are required**, not stylistic. The existing
  AppleJackRP deployment already carries them and works; its comment attributes
  that to Kubernetes wiring, but the real reason is the engine's terminal check.
- The bridge should stay on a ClusterIP behind a NetworkPolicy. `trusted` auth
  leans on the bridge being unreachable from anywhere but the game host, and
  Cellar refuses `trusted` on a non-loopback bind for that reason.
- Give the web UI a password if it is reachable from anywhere but the pod.
  Cellar refuses to start otherwise.
- MariaDB is a prerequisite. The cluster does not have one today.

---

## Notifications

Discord embeds coloured from the Applejack palette, plus a generic JSON sink.

```toml
[notify]
enabled = true
batch_seconds = 5
kinds = []          # empty means every notable kind
```

```sh
export CELLAR_DISCORD_WEBHOOK_URL='https://discord.com/api/webhooks/...'
export CELLAR_WEBHOOK_URL='https://example.invalid/hook'
```

**Batching is not politeness.** Discord rate-limits bursts and answers `429`;
thirty players joining at once is thirty messages without a coalescing window.
`batch_seconds` collapses them into one.

Webhook URLs are read from the environment, never logged, and redacted from
`cellar config`. Never echo one into a shell you are sharing; pipe it straight
into your secret store.

The kinds you can filter on are listed in
[Configuration](CONFIGURATION.md#notify).

---

## Updates

```toml
[update]
policy = "notify"        # off | notify | apply
only_when_empty = true
window_start_hour = 4
window_end_hour = 6
```

`notify` is the default: Cellar tells you an update exists and takes it only when
you ask. `apply` is hands-off.

Cellar also checks its own release feed in the background when
`[update].program_check` is enabled. The result appears in the Versions tab and
Cellar logs a notice when a newer program is available. Install it deliberately
with `cellar self-update`, then restart Cellar.

Three guards, all on by default:

- **`only_when_empty`.** An updater that restarts a populated server is worse
  than no updater. `cellar update --force` overrides it and announces itself.
- **The maintenance window.** Local hours. Equal values mean any time; `22`–`4`
  wraps midnight.
- **A dirty checkout is refused**, even by `--now`. Cellar will not throw away
  uncommitted work in the gamemode checkout.

`cellar version` shows the gamemode's build stamp, the git remote's HEAD and the
Steam build id side by side. That comparison is how the stale
`BuildVersion.g.cs` stamp in AppleJackRP was noticed.

Engine updates through SteamCMD are off by default and need `steam_dir` and
`steamcmd` set.

A locally-hosted MariaDB (`[mariadb]` `managed = true`) is not covered by any
of this: bumping `mariadb.version` and its pinned `mariadb.sha256` is a
manual, deliberate edit, the same reasoning `update.policy` never defaults to
tracking "latest". After bumping the version, run `cellar mariadb provision`
again; a major version jump against an existing data directory may also need
`mariadb-upgrade` run once by hand, which Cellar does not automate.

### Taking a new gamemode build: the runbook

The updater automates the middle of this. The edges stay an operator's job,
and the order is the point:

1. **Back up first.** `applejack_storage_backup` at the console copies every
   game document, journal included, in seconds and destroys nothing. If the
   bridge is on, dump the database too (see [Backups](#backups)). The game
   migrates its documents forward on the next boot and never backward, so the
   backup taken before the upgrade is the only way to run the old build again
   with its old data.
2. **Stop gracefully.** `cellar update --now` does this itself: `quit` at the
   console, wait out `supervisor.graceful_timeout_seconds`, update, start
   again. Stopping by hand from the dashboard first is equivalent. Never kill
   the process; the engine has no SIGTERM handler, so a kill skips the convar
   save and the Steam logoff.
3. **Update.** Development mode: `cellar update` pulls the gamemode checkout
   (a dirty checkout is refused, even by `--now`), then re-run the runtime
   sync so the external copy matches the checkout. Published mode: publish the
   new package version from the s&box editor; the server resolves the package
   at launch, so the restart itself is the update and there is no checkout to
   pull (`update_gamemode = false` in the published profiles). Engine: through
   SteamCMD, only when `steam_dir` and `steamcmd` are configured.
4. **Migrations run themselves; know whose they are.** The game owns its
   document migrations and applies them when it boots. Cellar's own `srv_`
   tables migrate on start (`database.migrate_on_start`, or `cellar db
   migrate` by hand), and with `schema_owner = "gamemode"` Cellar applies
   nothing to game tables at all. There is no separate migrate step to run,
   which is exactly why step 1 is not optional.
5. **Verify, then walk away.** `cellar doctor` before the start catches the
   config class of failure: `server.data_dir mode`, `project Libraries`, the
   executable. After the start: `/readyz` answers `200`, `cellar version`
   shows the new build stamp beside the remote HEAD (the dashboard's Releases
   tab runs the same drift check), the unparsed-line counter stays flat, and
   one real client joins and sees their character. If any of that fails: stop,
   `applejack_storage_restore <name>` from step 1's backup, start the previous
   build, and investigate offline.

---

## Backups

What actually matters is `aj_document`. Everything with an `srv_` prefix is
operational and regenerable.

```sh
mysqldump cellar aj_document aj_document_revision > characters.sql
```

Or per document, which is easier to reason about:

```sh
cellar doc ls characters/ | while read -r key; do
    cellar doc get "$key" > "backup/$(basename "$key")"
done
```

`aj_document_revision` is append-only, so it grows. It is also the only thing
that can answer "who overwrote my character", so prune it deliberately or not at
all. `cellar db prune` does not touch it.

A locally-hosted MariaDB's `mariadb.data_dir` is an ordinary MariaDB data
directory; standard tooling applies unchanged. Stop `cellar run` first (a
filesystem snapshot of a live InnoDB data directory is not a consistent
backup), or use `mariadb-dump` against the running instance the same as
against any other MySQL/MariaDB server.

---

## Retention

```toml
[database]
event_retention_days = 90
```

```sh
cellar db prune
```

Deletes from `srv_event` only. Run it from a CronJob if you want it regular;
Cellar does not schedule it for you.

---

## What to watch

| Signal | Where | Means |
| --- | --- | --- |
| Unparsed line count | `cellar config`, web UI | The engine changed a log string. The grammar needs updating. |
| Bridge health | `/readyz`, webhooks | The database is unreachable. Writes are being refused, not lost. |
| Crash loop | logs, webhooks | `crash_loop_threshold` failures inside `window`. Cellar has given up restarting. |
| Revision conflicts | `cellar db status` | Real concurrent writers exist. Relevant before enabling optimistic concurrency. |
| MariaDB lamp | web UI, `cellar mariadb status` | Only present with `[mariadb]` `managed = true`. Down or crash-looping means the game server is about to lose its own database, not just the browser. |
| Anti-cheat | web UI, `/api/status` | Shows detected VAC, Easy Anti-Cheat, or BattlEye evidence. `unknown` means the engine log made no claim. An unauthenticated Steam fallback is shown as VAC disabled. |
| AppleJack build drift | web UI, Releases tab | Compares the running `BuildVersion.g.cs` commit with the current `origin/main` tip. `out of sync` is a real alert, not a package-version guess. |

The Dispatch console has a view selector for command results, background events,
and errors. This keeps output from Precinct shortcuts visible without mixing it
with the persistent engine stream. Console view and filters are saved per
browser.

Settings snapshots can be imported from the Settings tab. Select a TOML or YAML
file, preview the planned changes, then apply them. Cellar sends only the named
changes through the live gamemode catalogue and never writes the source file or
the AppleJackRP checkout.

## Five useful features to add next

Comparable managers commonly provide these features. Cellar already has the
primitives for the first steps, so they are the next product backlog:

1. Durable scheduled tasks with missed-run policy and execution history.
2. Game-data backup and restore with retention and pre-update snapshots.
3. Scoped roles, expiring service tokens, and durable audit records.
4. Persisted monitoring history with threshold and recovery alert policies.
5. Signed inbound integrations mapped to named safe actions.

The comparison used official product documentation from
[Pterodactyl](https://pterodactyl.io/panel/1.0/additional_configuration.html),
[AMP](https://www.cubecoders.com/AMP/TQ),
[LinuxGSM](https://docs.linuxgsm.com/commands/monitor),
[GameAP](https://docs.gameap.com/), and
[Crafty Controller](https://docs.craftycontrol.com/pages/user-guide/open-metrics/).

The unparsed counter is worth a dashboard. A parser that quietly stops matching
is the failure mode this whole design is built against: an unmatched line is
recorded as `Unparsed` and counted, never silently dropped.

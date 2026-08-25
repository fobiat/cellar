# Operations

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

`/readyz` is derived from the log line in `server.ready_pattern`, which defaults
to AppleJackRP's own `Lobby created - session is joinable` from
`Code/NetworkBootstrap.cs`. Its body says what it knows, for example
`running, 2 player(s)`.

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

The unparsed counter is worth a dashboard. A parser that quietly stops matching
is the failure mode this whole design is built against: an unmatched line is
recorded as `Unparsed` and counted, never silently dropped.

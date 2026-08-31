# Troubleshooting

Run `cellar doctor` first. It checks most of what follows and reports it in a
sentence rather than as a startup failure.

---

## Commands do nothing, and nothing is logged

**Cause: the server has no console, because it has no terminal.**

The engine builds its console only when its output is *not* redirected:

```csharp
if ( !Console.IsOutputRedirected )
{
    console = new DedicatedServerConsole();
}
```

With a pipe, the console object is never constructed, so **nothing reads stdin at
all**. There is no alternate input path. Commands go into a void.

Fixes:

- Docker: `docker run -it`, or `tty: true` in a compose file.
- Kubernetes: **both** `stdin: true` and `tty: true` on the container.
- systemd: `StandardInput=tty` and a `TTYPath`, or run it under a terminal
  multiplexer.

Cellar always spawns the child on a pseudo-terminal, so this only bites when
Cellar's *own* environment has no TTY to inherit.

## `cellar settings` says it cannot reach the server

`settings` reaches a running `cellar run` over its web API. Check, in order:

1. `web.enabled = true` in the config that `cellar run` is using.
2. The same config file is passed to both. `-c` defaults to `./cellar.toml`, so
   two different working directories means two different servers.
3. `CELLAR_WEB_PASSWORD` is set, if that server's web UI has a password. The
   plain password, not the hash.

## Cellar refuses to start

By design, rather than running in a state that confuses you later.

| Message | Fix |
| --- | --- |
| `database.enabled needs CELLAR_DATABASE_URL` | Set it, or `enabled = false`. |
| `bridge.auth = "shared_secret" needs CELLAR_BRIDGE_SECRET` | Set it. |
| `web.enabled on a non-loopback address needs CELLAR_WEB_PASSWORD_HASH` | Run `cellar hash-password`. Not overridable: the console behind that page runs at full engine privilege. |
| `facepunch` auth refused | There is no published endpoint to verify a platform token. Use `trusted` on loopback, or `shared_secret`. |
| `trusted` auth on a non-loopback bind | `trusted` does not verify anything; it relies on being unreachable. Bind loopback or change mode. |

## The server starts, exits within seconds, and Cellar restarts it forever

The map never loads, and every convar Cellar sends comes back as
`Unknown Command`. The gamemode was never loaded, so there was nothing to
receive them, and `restart = "on_failure"` retries into the same wall until the
crash-loop threshold stops it.

Read the engine's own log rather than Cellar's status. The cause is usually near
the top, tagged `[Bootstrap]`:

```
Could not find a part of the path '...\applejackrp-runtime\Libraries'.
   at Sandbox.GameInstanceDll.StartGame(String gameIdent, String mapIdent)
```

`StartGame` enumerates `<project>/Libraries` and throws when it is absent. Git
does not track empty directories, so a checkout that never committed a
placeholder there loses it on clone or `git clean`, and a `robocopy /MIR` sync
copies the absence into the runtime tree. Create the directory, and commit a
`.gitkeep` in it so it survives the next clone.

`cellar doctor` reports this as `project Libraries`.

## The gamemode still writes to local files

The bridge is running and nothing errors, but nothing lands in the database.

**Cause: `server.data_dir` is unset**, so Cellar never wrote `hosting.json`, so
the gamemode never learned there was a bridge. It falls back to local storage
without complaint.

Set `data_dir` to the engine's data directory, the one beside `features.json`
and `permissions.json`. `cellar doctor` checks this.

## The gamemode cannot reach the bridge

Symptoms: `StorageDirector` reports `Unavailable`, nothing in `aj_document`.

Causes, most likely first:

1. **`-allowlocalhttp` missing.** Cellar adds it when the bridge is enabled. If
   you launch the server another way, the engine blocks every private and
   loopback address and you get silence.
2. **`public_url` redirects.** The engine strips `Authorization` on every
   redirect hop. The URL must be final.
3. **Platform auth is unavailable during local development.** Cellar writes a
   loopback-only `apiKey` into `hosting.json` for trusted bridges, so restart
   the managed server after upgrading Cellar to regenerate that file.
4. **Answering too slowly.** The client times out at 3s for reads, 5s for
   writes, and opens its circuit breaker after three failures.

## Characters vanish, or reset to empty

This is the failure the whole bridge design guards against, so check it
carefully.

**`404` must be the only status that means "absent".** If any other error is
being mapped to absent, then a player joins during a database blip, their roster
reads as empty, the gamemode treats them as new, and the empty roster is written
back over their character. Nothing errors.

Verify:

```sh
curl -i -H 'Authorization: Bearer test' http://127.0.0.1:8080/v1/doc/nope.json
```

Exactly `404`. If a database outage produces `404` rather than `500`, stop the
server and fix that before anyone joins.

`cellar doc history <key>` shows every revision and who wrote it.

**If they vanished right after a mode switch, look at `server.data_dir` before
suspecting the database.** Development and published mode read different
directories, `applejackrp#local` and `applejackrp`, and a profile pointing at
the wrong one shows an empty roster with nothing logged. `cellar doctor` reports
that as `server.data_dir mode`.

## MariaDB and MySQL differ

Cellar supports both, but they are not the same database and these differences
each cost real time to find:

| Symptom | Cause |
| --- | --- |
| `CAST(? AS JSON)` rejected | MySQL-only syntax. Cellar binds plain text instead. |
| `VARCHAR` arrives as `VARBINARY` | An explicit `utf8mb4_bin` collation does this on MariaDB. The schema sets no explicit collation. |
| A `JSON` column arrives as a BLOB | MariaDB returns it as bytes. Cellar decodes from bytes. |
| `BIGINT UNSIGNED` fails to decode | It decodes only to `u64`, never `i64`. |

## The player count is always zero in the server browser

Expected. The engine calls `SteamGameServer_Init` and sets name, map and max
players, so `A2S_INFO` answers, but **nothing ever calls `BUpdateUserData`**,
which is what Steam derives the player count and `A2S_PLAYER` list from.

Cellar reads players from the log instead. The browser figure is not something
Cellar can fix from outside the gamemode.

## `+maxplayers` has no effect

Also expected. There is no such convar and no such launch switch.
`LaunchArguments.MaxPlayers` exists but nothing on the command line sets it; the
real ceiling comes from `Metadata.MaxPlayers` in the `.sbproj`.

AppleJackRP's old repository entrypoint used to pass `+maxplayers`, but the
engine ignores it. Cellar does not pass the flag.

## Uptime in the status bar is an hour ahead

That is the engine, and Cellar corrects it.

`DedicatedServerConsole.UpdateStatus` renders the hour as `{TotalHours:n0}`,
which **rounds** instead of truncating, while the minutes and seconds come from
`ToString("mm\:ss")` and are exact. So 40 minutes of uptime renders `1:40:00`,
and the bar is wrong for half of every hour.

The error is exactly `minutes >= 30`, so Cellar inverts it and reports true
uptime. What you see in a raw terminal is the engine's figure; what Cellar
reports is the real one.

## The unparsed line count is climbing

The engine changed a log string, and the grammar has not caught up.

An unmatched line is recorded as `Event::Unparsed` and counted rather than
dropped, precisely so this is visible instead of silent. The parser lives in one
file, `crates/cellar-core/src/grammar.rs`, with a fixture corpus, so a change is
localised.

## Self-update says there are no releases

Public releases need no token. A private fork answers `404` to an anonymous
caller rather than `403`. Cellar distinguishes the two cases in its message:
"no releases published" is different from "no releases visible to you".

Set `CELLAR_GITHUB_TOKEN` or `GITHUB_TOKEN`.

## The installer downloads the wrong file, or 404s

Fixed in v0.1.1. The Linux installer used to pick the wrong asset id by matching
across comma-split JSON. If you are on an older build, install manually from the
releases page or re-run the current installer script.

## arm64 is not supported

No arm64 build is published, and the installer says so rather than downloading
something that will not run.

The dedicated server is a Windows x86_64 binary run under Wine, so an arm64 host
cannot run what Cellar supervises. Build the CLI from source with
`cargo build --release` if you want it on an arm64 box for its own sake.

---

## What has not been proven

Stated rather than omitted. A claim you cannot check is worse than a gap you can
see.

- **Gamemode telemetry is not built.** Balances, jobs, positions and feature
  state are carried by no log line. That needs a `Code/` file in AppleJackRP,
  which is expensive there, so it is deliberately last.

- **Whether A2S reports players once players are connected.** The query port
  answers with the right hostname and max players, an empty map and appid 0.
  `SteamGameServer_Init` is called with a real query port but nothing calls
  `BUpdateUserData`, which is where Steam derives the count from, so the count
  is expected to stay zero. Settling it needs a real client connected.

- **Where the engine caches packages downloaded from sbox.game.** Undocumented,
  and it has already caused one failure: a development run stopped on a missing
  `facepunch.sbox_content` that is in neither the engine source, the gamemode,
  nor the Steam install. Until the location is known there is no offline story
  and no way to pin a package version.

### Proven since, and worth recording as proven

- **`sbox-server.exe` under Wine.** Proven 2026-08-30 on Arch: Cellar spawned
  it, typed a command into the pty, the engine parsed it and answered into the
  log, and SIGTERM to Cellar produced a full `Source2Shutdown` rather than a
  kill. This was the assumption everything command-shaped rested on.

- **CI runs.** GitHub Actions was blocked on account billing until 2026-08-27.
  The local gate (`cargo build`, `cargo test`, `cargo clippy --all-targets --
  -D warnings`, `cargo fmt --check`, `bash scripts/check-docs.sh`) is still what
  runs before every commit.

- **Installing the dedicated server needs no Steam credential.** Verified
  2026-08-31: `steamcmd +login anonymous +app_update 1892930 validate` answers
  `Success! App '1892930' fully installed`, 4.9GB. The recorded blocker was
  about app 590830, the paid client, which is a different app and does not carry
  a dedicated server at all.

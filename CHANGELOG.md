# Changelog

All notable changes to Cellar are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **A supervisor that can actually reach the server's console.** s&box only
  builds its dedicated console when its output is not redirected
  (`Launcher.cs`), and reads input with `Console.ReadKey()`, which throws on a
  redirected stream. A pipe therefore gets no console at all. Cellar spawns the
  server on a pseudo-terminal, which satisfies both checks, and that gives it
  `ConVarSystem.Run` at full privilege: native `quit`, `kick` and `status`, plus
  all 95 `applejack_*` commands, with no gamemode change.
- **The persistence bridge, implemented.** `GET`/`PUT`/`HEAD /v1/doc/{key}` over
  MySQL, to the contract `Documentation/Design/20_PERSISTENCE.md` §6.3 specifies
  and `Code/Storage/HostedDocumentStore.cs` already speaks. Every document is
  versioned and every write is kept, so a bad write is recoverable. 404 is the
  only status mapped to absent, which is §4.1's whole point.
- **Readiness and liveness probes**, answering the question the deployment
  manifest asked in a comment. `/readyz` is derived from the readiness line in
  the engine's log, not from a port being open, and is 503 while starting,
  backing off or crash-looping.
- **A graceful stop.** The engine installs no SIGTERM handler anywhere, so a
  kill skips all nine of its shutdown steps including the Steam logoff and the
  convar save. Cellar sends `quit` through the console, waits, and escalates to
  a kill only after the grace period, saying so when it does.
- **Three interfaces over one snapshot**: a CLI, a ratatui dashboard and a web
  UI, so they cannot disagree about what the server is doing.
- **A read-only database browser** and a bridge document browser with revision
  history, in the web UI. Reads are enforced twice: a statement-shape check, and
  the grant the browser's database user holds.
- **Version checking and a hands-off updater.** The gamemode's build stamp, the
  git checkout against its remote, and the Steam build id of the dedicated
  server. The updater defaults to `notify`, refuses a dirty checkout, refuses
  while anybody is connected, and respects a maintenance window.
- **The server's configuration as a file.** The 41 features, the 7 catalogued
  settings and the engine's convars captured to TOML or YAML, diffed against a
  running server, and applied to one. A partial file sets what it names and
  never resets what it omits, and the live catalogue decides what is writable
  rather than the file: a `core` feature is refused and a `boot` one is flagged
  as needing a restart. `cellar settings dump|diff|apply|set`, and the same
  thing clickable in the web UI.
- **Batched Discord webhooks** in the Applejack palette, with player names
  escaped so a display name cannot format the channel.
- **`cellar-fake-server`**, a stand-in that emits the engine's real log formats,
  so the whole supervisor can be tested in CI with no Steam, no Wine and no
  s&box.

### Fixed

- **Every log line was counted twice.** The console and the log file carry the
  same content, and both were parsed. The log file is now authoritative, with
  the console held briefly at startup so the polled tailer gets a turn, and
  falling back to the console only if the log file never appears.
- **A command came back as the first line of its own reply**, because a pty
  echoes what is typed into it.
- **A clean shutdown was reported as a signal kill.** `try_wait` reports an exit
  status once; the graceful stop consumed it and the caller then asked again and
  got nothing.
- **A log file created after Cellar started was skipped entirely**, because the
  tailer seeked to the end on first open regardless of whether the file had
  existed beforehand.
- **`BIGINT UNSIGNED` columns rendered as their type name** in the database
  browser, which is every id and every revision in the schema.
- **MariaDB rejected the schema and the writes.** `CAST(? AS JSON)` is
  MySQL-only, and an explicit `utf8mb4_bin` collation makes every `VARCHAR` come
  back as `VARBINARY` over the wire.

### Notes

- `+maxplayers` is not a convar or a launch switch in s&box. The existing
  `entrypoint.sh` passes it and it does nothing; the real ceiling comes from
  `applejackrp.sbproj`'s `Metadata.MaxPlayers`. Cellar does not pass it.
- Steam A2S answers on the query port but reports **zero players**, because
  nothing in the engine calls `BUpdateUserData`. It is a liveness signal, not a
  roster.
- The bridge cannot verify `Auth.GetToken` tokens: Facepunch publishes no
  introspection endpoint. `trusted` mode says so rather than implying otherwise,
  and refuses to bind anything but loopback.

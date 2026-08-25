# Changelog

All notable changes to Cellar are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4] - 2026-08-25

### Added

- **Cellar can host its own MariaDB.** `[mariadb]` (`managed = true`) downloads
  an official MariaDB release, verifies it against a pinned checksum,
  initializes a data directory and supervises `mariadbd` as a child process of
  `cellar run`, the same way Cellar already supervises the game server. Built
  for machines with no MySQL already available and no Docker to run one in.
  `cellar mariadb provision` does the one-time setup and prints
  `CELLAR_DATABASE_URL`; `cellar mariadb status` reports what is there. The
  existing read-only database browser in the web UI needed no changes: it
  already just reads whatever `database.url` points at. See
  [Configuration](docs/CONFIGURATION.md#mariadb) and
  [Installation](docs/INSTALLATION.md#hosting-mariadb-locally).

- **Server control plane integration.** The dashboard now manages the
  AppleJack invite gate and SteamID64 allowlist, tails engine logs, reports
  spawn and map health, and exposes build and publish actions.
- **Database backups.** `cellar db backup` creates timestamped logical dumps;
  `[backup]` can schedule them and prune old files.
- **Windows tray controls.** `Cellar-Tray.ps1` provides start, stop, restart,
  exit and web UI actions from the notification area.
- **AppleJack release wrappers.** The Windows scripts open the project in the
  installed s&box editor for build and editor-authenticated publishing.

## [0.1.3] - 2026-08-25

### Fixed

- **The Windows installer could never verify a download.** `install.ps1` asked
  for the checksum with `Accept: application/octet-stream`, which makes
  `Invoke-WebRequest` return `.Content` as a `Byte[]`. `-split` then split the
  array rather than the text, so the parsed digest was the first byte's decimal
  value, `57`, and every install died on a checksum mismatch that looked like a
  corrupt download. The checksum is now written to a file and read back as
  text. Present since v0.1.1; the published artifacts were always correct.
- **A checksum that fails to parse now says so.** `install.ps1` applies the same
  64 hex digit guard `cellar self-update` already had, so a parsing fault
  reports itself instead of surfacing as a mismatch and sending you to look at
  the download.

## [0.1.2] - 2026-08-25

### Fixed

- **The status bar is two lines, and only one of them was read.**
  `SetStatus(1, ..)` carries the name, player counter, clock and Network
  timing; `SetStatus(2, ..)` carries Physics, NavMesh, Animation and Update and
  has no player counter at all. The parser anchored on the counter, so line B
  never matched and the frame timings never arrived. The two fragments are now
  parsed separately and merged.
- **A command reply is bracketed exactly, so it no longer waits out a timer.**
  `ConsoleInput.OnEnter` writes `"> " + inputString` before `OnInputText`, and
  `RedrawInputLine` repaints the status block after `ConVarSystem.Run` returns,
  so the reply is the lines between those two markers. The 750ms window is
  demoted to a 3s backstop for a console that never echoes: 0.15s bracketed
  against the fake server, 3.23s only when the echo is suppressed.
- **Uptime was reported an hour ahead.** The engine renders the bar with
  `{TotalHours:n0}`, which rounds where `mm\:ss` truncates, so 40 minutes shows
  as `1:40:00`. The error is exactly `minutes >= 30`, so it is inverted on the
  way in and Cellar reports true uptime.
- **`aarch64-unknown-linux` was offered and has never existed.** `install.sh`
  and `current_target` both named a target no release carries, turning an
  unsupported platform into a 404 midway through a download. The installer now
  says the server is a Windows x86_64 binary under Wine and points at
  `cargo build`.

### Added

- **Eight guides under `docs/`**: QUICKSTART, INSTALLATION, CONFIGURATION, CLI,
  BRIDGE, OPERATIONS, TROUBLESHOOTING and ARCHITECTURE. The README is cut back
  to a front door that links to them instead of restating them. Written against
  the shipped binary, which caught four things the docs would have stated
  wrongly, including that `web.enabled` defaults to false and that `/healthz`
  and `/readyz` are served on both the bridge and the web listener.
- **`scripts/check-docs.sh`**, a documentation gate in CI: every relative `.md`
  link resolves, every `#anchor` matches a real heading, and no em dash appears
  anywhere. Falsified on a deliberately broken link and anchor before it was
  trusted.

### Changed

- **A missing value says what it means.** An unknown revision author prints
  `unknown` and an unknown player ceiling prints `?`, where both previously
  printed an em dash.
- **The release build runs on a self-hosted runner.** Actions minutes are billed
  on a private repository and every hosted run has failed before starting, so
  `Release` routes to `vars.CI_RUNNER` the way `CI` already did, and the Windows
  binaries are cross-compiled there in the container `scripts/` already
  provided rather than on a Windows runner.

## [0.1.1] - 2026-08-25

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

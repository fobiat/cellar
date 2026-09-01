# Changelog

All notable changes to Cellar are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **More than one server in one process.** `[instances.<id>]` alongside
  `[server]`, never both. Each gets its own log, data directory, scope, ports,
  Wine prefix and Discord attribution; a crash loop in one leaves the other
  running and `/readyz` green, because it speaks only for instances marked
  `required`. The dashboard grows an instance strip and hash routing
  (`#/i/<id>/<tab>`), the CLI a `--instance` flag, and MCP an `instance`
  argument plus a `cellar_instances` tool. A single-`[server]` config is
  untouched: no strip, no query parameter, and its documents do not move.

  It is **one install tree per instance**. `FACEPUNCH_ENGINE` relocates neither
  `logs/` nor `data/`, from the process environment or from the Wine registry;
  both follow the executable's own directory, and `server.working_dir` moves
  neither. Measured against a real `sbox-server.exe`; the table is in
  `docs/ARCHITECTURE.md`.

- **Gamemode profiles.** `[profile]`, or `profile_file` pointing at a sibling,
  carrying `ready_pattern`, `convar_prefix`, `map`, `[[command]]` and
  `[[check]]`. Five things were hardcoded to AppleJackRP: the readiness line,
  the log category heuristic, a doctor check that grepped one C# file by path,
  thirteen command buttons in the web UI, and `/api/settings` sending
  `applejack_features` to whatever was running. A gamemode Cellar has never
  heard of now gets all five by writing twenty lines of TOML.

- **`cellar install`**, which fetches the s&box dedicated server itself. App
  1892930 is free and anonymous login works, so this needs no Steam account.

- **`cellar db restore`**, `cellar db backups`, and their endpoints. A backup
  feature without a restore is decorative. Restore refuses while a Cellar is
  answering, because a write landing mid-restore lands in a table that is about
  to be dropped.

- **An activity screen** over `srv_command` and `srv_event`, which Cellar has
  always written and never shown. The console runs at full engine privilege, so
  the command audit is the only account of who used it.

- **A diagnostics screen** running the same checks as `cellar doctor`, from the
  same crate, plus the live half: supervisor state, restart counts, last exit,
  each instance's resolved readiness line, and the unparsed-line count with the
  last twenty lines verbatim.

- **A job register.** Backups, the update check, the program update check and
  event retention now report when they last ran, whether it worked and when the
  next one is due, with a "run now" per job, over `GET /api/jobs` and a panel on
  the Diagnostics tab.

- **`backup.verify`**, on by default, reads each dump back before counting it
  and before retention may delete an older one. Plus `backup.copy_to` for a
  second copy on another disk, and `backup.before_update` for a snapshot taken
  immediately before a game update is applied.

- **A start timeout**, `supervisor.start_timeout_seconds`, and a `unhealthy`
  state for a server whose process is alive and whose readiness never arrived.
  It never kills and never restarts; it stops the two being the same word.

- Doctor checks for a port already in use, disk headroom, another Cellar
  already bound to this address, and the **Steam app identity**: an install of
  590830 is the paid client and editor, not the dedicated server, and version
  reporting cannot work against it.

- A theme control offering Dark, Light and System, with the choice remembered.

- **Which server this is, above the console it belongs to.** A collapsible
  header carrying the mode, gamemode, map, players, uptime, restart policy,
  document scope and the join address with a copy button. Not a tab: a screen
  whose only job is to restate the instance strip is a screen that gets opened
  once.

- **Average frame timings, a measured frame total, and the tick ceiling it
  implies.** The engine's status bar reports stage times and no frame rate, so
  Cellar reports their sum rather than inventing an FPS, and labels the ceiling
  as what the sum alone would allow. Averages run over the last five minutes of
  samples, because one reading is a spike and a spike is what an operator sees
  when they happen to look.

- **Playtime and visits beside every connected player.** The roster and the
  recorded history were two separate tabs, so an operator deciding whether to
  kick somebody could not see that they had forty hours here. They are one
  screen now, joined on the Steam ID, and a dash means the operations database
  is off rather than a first visit.

### Fixed

- **The light theme, which never worked.** The `ink` token carried the dark
  value in both themes, so light mode painted `#201F1D` body text on a
  `#0E0F11` ground at 1.15:1. The palette's contrast tests now hold both halves
  of every token to WCAG AA on every ground; they only ever checked the dark
  half, which is how it stayed broken while its tests passed.

- **`/healthz` did not identify itself as Cellar.** It answers `ok`, and the
  probe that decides whether an address is held by another Cellar sniffed that
  body for `"cellar"` or `"state"`, so it never matched. Two things depended on
  it and both were inert: doctor's "another Cellar is already bound" warning,
  and the refusal that stops `cellar db restore` running while a supervised
  server writes to the database it is about to replace. There is an `x-cellar`
  header now.

- **Event retention never ran.** `database.event_retention_days` defaults to 90
  and had no loop at all; only the manual `cellar db prune` ever acted on it, so
  a long-running deployment's recorded events grew without bound.

- **Notifications followed the primary instance only**, so on a two-instance
  deployment a crash on the second server notified nobody.

- **`deploy/cellar.toml` and the Kubernetes manifest could never have passed
  readiness.** Both ran `facepunch.sandbox` with AppleJackRP's readiness line,
  which that gamemode never logs.

- **The dashboard was mouse-only.** No labels, no tab roles, no `tabindex`
  anywhere. Every input relied on a placeholder, which disappears on focus.
  Every status was a coloured dot with no distinguishing shape, so the screen
  read identically in greyscale and to a red-green colour deficiency.

- `/api/status` reports a stopped state with the exit that produced it, rather
  than `"server": null`. After a kill the dashboard showed an absence instead of
  "killed, exit 137".

- The console renders one node per arriving line instead of rebuilding up to
  1500 elements per line, marks a gap when the socket drops and backfills it
  from the log file on reconnect with an integrity readout, and shows commands
  run from `cellar exec` or MCP, which it used to drop.

- `command_dispatched` and `command_replied` were broadcast and dropped by the
  browser, so a command run from anywhere but the web console was invisible.

- The player ceiling comes from the project's `Metadata.MaxPlayers`, the only
  place it exists. `+maxplayers` is not a convar and not a launch switch.

- `record_events` is keyed by instance. It held one session id, so on a merged
  stream one instance's exit closed the other's session row, with both writes
  succeeding and nothing reporting it.

- A missing server executable is reported as a missing path rather than backed
  off into five times and reported as a crash loop.

- `setLamp` assigned `className` outright and dropped `.connection` from the
  header lamp, so a phone showed a large "live" label designed to be invisible.

- The table search and sort ran on the two-second status refresh, re-appending
  every row of six tables, which blurred anything focused inside one. Typing a
  convar value into the Settings table lost focus twice a second.

- `game_data_dir` prefers `server.data_dir`, so the Access panel is no longer
  silently blind in development mode.

- `level_min` on `/api/logs`, so the live view's threshold filter and the
  full-log search agree.

### Changed

- **Twelve tabs became nine.** Roster, Registry and Access are one **Players**
  tab with Connected, History and Access inside it; Configs, the convar tables
  that were living in Settings and the build controls that were living in
  Releases are one **Config** tab with Profile, Convars and Build. Settings
  keeps what is Cellar's own: the web UI's authentication posture, Cellar's own
  version, and shutting it down. Every old route still resolves, now to the
  sub-tab its screen went to.

- **A screen that shows shared data now says whose it is.** The database
  browser names every instance on that one connection, because switching server
  in the strip and seeing the same tables reads as two databases. The access
  panel names the two files it writes and whether another instance reads the
  same directory. Records names its bridge scope.

- **SteamIDs are strings in every JSON response**, because JSON numbers are
  doubles and every real SteamID64 is above 2^53. Deserialising still accepts a
  number, so an older recorded payload reads back.

- Report sizes in decimal KB/MB/GB rather than KiB/MiB/GiB, in the web UI and
  in the TUI and CLI both. The two error strings naming a fixed binary limit
  (the 512 KiB settings import and the 2 MiB MCP response cap) keep their
  units, because the limits themselves are unchanged and relabelling them
  alone would misstate them.
- Move the mode, gamemode and profile readout, the restart policy and the
  connection lamp to the right of the masthead, leaving the Cellar version and
  build commit on the left. Dropped the "server control" tagline.

### Fixed

- **Both charts stretched their own coordinate space.** They carried a fixed
  `viewBox` and `preserveAspectRatio="none"`, so a 320-unit box painted across
  a 1180px panel stretched every unit horizontally and left the height alone.
  The line survived that; the axis labels came out nearly four times as wide as
  they were tall. The viewBox is the element's own pixel box now, and a chart
  drawn while its tab was hidden is redrawn when it appears.

- **An empty player history could not be told from a disabled one.** Both were
  an empty table. The status already carries which, so the empty state names
  `database.enabled` when that is the reason.

- Report uptime in whole seconds. Below a minute it carried three decimal
  places computed from a timestamp, so it repainted twice a second.

- **Every SteamID the dashboard showed was rounded.** They crossed as JSON
  numbers, and `JSON.parse` turns a number above 2^53 into the nearest double,
  which for a SteamID64 is a multiple of 16. Three players whose ids differed
  by one appeared as the same account, and the kick button sent that id back.
  A Rust test had guarded the u64 through serde and stopped one hop short of
  the reader that breaks.

- **A link naming both an instance and a tab dropped the tab.** Following
  `#/i/published/records` from another instance switched the server and stayed
  on whatever tab was open, so a route written into a runbook arrived
  half-applied.

- Confirm before kicking a player. It fired on the click, and the person is on
  a loading screen before the operator has finished reading the row.

- Stop the two Linux AppleJackRP profiles shipping `mariadb.managed = true`.
  A managed instance cannot work there, so both now ship `managed = false`
  with the reason beside it.
- Point the published AppleJackRP profiles at the published package's data
  directory. All three carried the `#local` leaf, which is the local `.sbproj`
  directory, so `hosting.json` was written where the game never read it and
  neither mode could see the other's characters, permissions or features.
- Point the published Facepunch Sandbox profiles at the published package's
  data directory too. `configs/facepunch-sandbox.toml`, `deploy/cellar.toml`
  and the Kubernetes ConfigMap all ran `facepunch.sandbox` with the `#local`
  leaf, and the profile-mode test only read `applejackrp*` files, so they
  slipped past it. The test now reads every shipped profile, the deploy config
  and the ConfigMap's embedded block.
- Read back a `hosting.json` whose `apiKey` is empty. The field is skipped on
  write when empty but had no serde `default`, so every locally provisioned
  document failed to deserialise. Written documents are unchanged.

### Added

- `cellar exec <command>` types a command into the running server's console and
  prints the reply. The console was already reachable from the web UI and, when
  `CELLAR_MCP_ENABLE_COMMAND=1`, over MCP, but not from the command line, so a
  script had to speak HTTP to `/api/exec` itself. `--file` runs a list one per
  line, skipping blanks and `#` comments; `--json` emits one object per command;
  `--keep-going` continues past a failure and still exits non-zero. It reuses
  the `LiveServer` client the `settings` subcommands already use, so it inherits
  their loopback resolution, `CELLAR_WEB_PASSWORD` handling and audit trail.

  Caveat worth knowing before scripting against it: the reply channel is
  best-effort. Under load the engine's own log output interleaves into the
  captured reply, replies can come back empty, and an unknown command is not an
  error, it returns whatever was on the console and exits 0. Assert positively
  on an expected token rather than treating a non-empty reply as success.

- `cellar doctor` checks that a local project's `Libraries` directory exists.
  The engine's `StartGame` enumerates it and throws when it is absent, and the
  server then exits to a bare console with no gamemode loaded, which
  `restart = "on_failure"` retries into the same wall.
- `cellar doctor` checks that `server.data_dir` agrees with the configured
  mode, so the `#local` mismatch above cannot come back unnoticed.
- `cellar mariadb provision` refuses on a non-Windows host instead of failing
  halfway. `release::archive_url` serves the winx64 zip unconditionally and
  every binary the provisioner spawns is an `.exe`, so a Linux host paid a
  400MB download, unpacked Windows binaries, and then died on the first exec
  with a bare `Permission denied (os error 13)` naming nothing. The error now
  says so and points at `mariadb.managed = false` plus `database.url_file`.
- `scripts/bootstrap-linux.sh`, which sets up a bare Linux host: curl, tar,
  rsync and Wine, then optionally steamcmd, the Windows .NET runtime inside the
  Wine prefix, and MariaDB, then Cellar itself. `install.sh` installs only
  Cellar, which suits a container image that already carries Wine; a bare
  machine is also missing everything Cellar supervises. It probes each
  component before touching anything, handles pacman, apt, dnf and zypper, and
  prints the exact command for anything it cannot install rather than failing
  the run. `--with-dotnet` uses Microsoft's own installer, because
  `sbox-server.dll` asks for `Microsoft.NETCore.App` 10.0.0 and winetricks
  stops at `dotnet9`.

## [0.1.13] - 2026-08-28

### Added

- Show `LIVE - DEVELOPMENT` or `LIVE - PUBLISHED` in the dashboard header.
- Show the active crash restart policy and make AppleJackRP published profiles
  explicitly restart failed server processes with backoff and crash-loop
  protection.

## [0.1.12] - 2026-08-28

### Fixed

- Keep private AppleJackRP source bundles in GitHub Actions artifacts instead
  of attaching them to Cellar's public release assets.

## [0.1.11] - 2026-08-28

### Added

- **Automatic local development sync.** The Windows AppleJackRP launcher now
  repairs the legacy database URL setting when its protected URL file exists,
  defaults to the local runtime profile, and watches source changes for a
  debounced sync and supervised restart.
- **Platform-specific AppleJackRP profiles.** Local and published Windows and
  Linux under-Wine profiles keep executable, project, data, log, database, and
  backup paths explicit.
- **Coordinated gamemode release bundles.** The release workflow can package an
  AppleJackRP source/build bundle with its generated build identity,
  `CHANGELOG.md`, and `GAMEMODE_CHANGELOG.md` as a private Actions artifact when
  the private repository token is configured.

## [0.1.10] - 2026-08-28

### Added

- **Build identity in the dashboard header.** The compact header now shows the
  installed Cellar version, the active profile scope, and the current
  AppleJackRP commit. Hover the muted metadata for the full commit value.
- **Normalized CPU telemetry.** The header now reports process CPU averaged
  across all logical cores. Monitoring keeps the raw per-core total, the
  normalized average, core count, process count, host CPU, memory, and network
  rates, with 0 to 100% grid scales on the CPU charts.
- **Local AppleJackRP development loop.** The Windows shortcut now defaults to
  the local `.sbproj` profile and can sync source changes into the runtime copy,
  then restart the supervised game without publishing to sbox.game.
- **Platform-specific profiles.** AppleJackRP local and published examples now
  have explicit Windows and Linux under-Wine files, so executable, project,
  data, log, database, and backup paths are not mixed between hosts.

### Fixed

- **The AppleJackRP tray returns when it is missing.** Clicking the desktop
  shortcut while Cellar is already healthy now recreates the tray process, and
  repeated starts do not create duplicate tray processes.
- **Settings put features first.** The Features panel now appears above the
  export and import controls so the live gameplay switches are immediately
  visible when Settings opens.

## [0.1.9] - 2026-08-28

### Fixed

- **AppleJackRP's Windows shortcut now starts the server.** The launcher uses a
  per-user config that reads the protected MariaDB URL file, validates the
  profile before starting, waits for Cellar health, and runs `doctor` after
  Cellar has started its managed database child.
- **Startup failures are visible.** Native Cellar errors now reach the launcher
  notice instead of being swallowed by a hidden PowerShell process.

## [0.1.8] - 2026-08-28

### Added

- **AppleJackRP one-click Windows startup.** The launcher validates the
  configuration, syncs the checked-out gamemode into its external runtime,
  starts the supervised server, waits for health, runs `cellar doctor`, and
  opens the dashboard.
- **Desktop and tray integration.** The shortcut installer creates an
  AppleJackRP desktop shortcut, a startup tray shortcut, and uses the bundled
  Cellar AppleJackRP icon for both surfaces.
- **Prompted update checks.** The launcher checks Cellar and AppleJackRP
  updates automatically, asks before applying either one, and refuses to apply
  a game update while the server is running.
- **Feature planning research.** Added a ten-item, source-backed AppleJackRP
  feature opportunity report covering onboarding, property controls, stats,
  presence, input, performance, notifications, and seasonal rankings.

### Fixed

- **Release packaging no longer names a missing license file.** Archives now
  contain the license that is actually present in the repository.

## [0.1.7] - 2026-08-27

### Added

- **Public deployment guidance.** Added a security policy, contribution guide,
  tagged installer examples, and GitHub issue routing for the open-source
  repository.
- **Safer hosted web configuration.** Non-loopback web binds now require an
  explicit TLS reverse-proxy acknowledgement and Secure cookies.

### Changed

- **Gamemode-owned databases remain the default.** Secret values are accepted
  from the environment only, and file-based secret fields are rejected.
- **Login attempts are throttled.** Failed operator logins are capped per
  process to reduce password-guessing and Argon2 CPU exhaustion.
- **Public pull requests use GitHub-hosted CI runners.** Maintainer-only runs
  may still use configured self-hosted runners.

### Fixed

- **Installer documentation no longer pipes mutable `main` scripts into a
  shell.** It downloads a tagged script, shows it before execution, and pins
  the installed release to the same tag.

- **A resource-sampler test failed on every native Windows run.** It used pid
  0 as a stand-in for a dead process, but Windows exposes pid 0 as the System
  Idle Process, so `sysinfo` reported it alive. The test now spawns a real
  child and samples its pid after it exits.

## [0.1.6] - 2026-08-25

### Fixed

- **Cellar can now exit cleanly for upgrades.** The tray and Windows installer
  use the new exit control, which gracefully stops the game and MariaDB before
  releasing the executable lock. Older managers fall back to the compatibility
  stop path.

## [0.1.5] - 2026-08-25

### Fixed

- **Windows reinstall no longer fails on a stale executable backup.** The
  installer now stops the matching Cellar process, removes an old backup with
  an actionable error path, and falls back to a unique retired filename when
  that backup is locked.

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

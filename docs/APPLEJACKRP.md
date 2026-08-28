# AppleJackRP configuration

Cellar is the server manager for AppleJackRP. The AppleJackRP repository keeps
the game project and gameplay code; Cellar owns the dedicated-server process,
the local MariaDB process, the document bridge, the operator dashboard and
restart policy.

Start with the checked-in platform profile: [Windows local](../configs/applejackrp-windows.toml),
[Linux local](../configs/applejackrp-linux.toml), [Windows published](../configs/applejackrp-public-windows.toml),
or [Linux published](../configs/applejackrp-public-linux.toml). Copy the matching
file to the Cellar config path and change only the paths that differ on the host.
The Windows installer uses `%ProgramData%\Cellar\cellar.toml`.

The config uses an external runtime copy of the local `.sbproj`, so the
dedicated server's s&box process does not write `.sbox` state into the
checkout. Sync the repository with
`tools\sync-cellar-runtime.ps1` before starting and after source changes. The
Windows development shortcut can run this sync automatically and restart the
supervised game after source changes with `-Watch`. This is a restart-based
development loop, not editor assembly hotload. The s&box editor remains the
place for true in-session code hotload. To
run a published package, remove `server.project` and set `server.game` plus
`server.map`. Keep `server.data_dir` pointed at the same package data directory
that contains `features.json` and `permissions.json`; Cellar writes
`hosting.json` there before launching the game.

Before the first run, provision the managed MariaDB once. The shortcut and the
public profile use the protected URL file below, so the password does not go
into `cellar.toml` or a shortcut argument:

```powershell
cd C:\Users\Shadow\Desktop\Projects\AppleJackRP-sandbox
.\tools\sync-cellar-runtime.ps1
cellar mariadb provision --config "$env:ProgramData\Cellar\cellar.toml"
# Save the printed URL, without its surrounding quotes, to:
# C:\ProgramData\Cellar\external-database.url
cellar run --config "$env:ProgramData\Cellar\cellar.toml"
```

The one-click launcher keeps its copy of this profile at
`%LOCALAPPDATA%\AppleJackRP\cellar.toml` and reads the same protected URL file.
That avoids requiring administrator access to change the machine-wide config.

With Cellar running, use a second shell for `cellar doctor --config
"$env:ProgramData\Cellar\cellar.toml"`. The doctor command checks the live
managed database as well as the server configuration.

The dashboard is at `http://127.0.0.1:8081`. Use `/readyz` for the game-server
readiness probe and `/healthz` for the Cellar process liveness probe.
The dashboard header keeps a compact record of the active mode, profile,
installed Cellar version, and AppleJackRP commit; hover the muted build marker
for the full commit. The Configs tab has explicit Use Development mode and Use
Published mode actions. Development mode is the local `.sbproj` path and the
watcher-backed sync/restart loop. Published mode is the `sbox.game` package and
does not use local source hot reload.

The checked-in published profiles run the published
package with `thieves.rpdowntown3t`, so the s&box lobby can resolve the game and
map thumbnails. The local project profile is for development and does not have
published package branding. Switch profiles from Cellar's Configs tab, or copy
the public profile beside the active config before starting Cellar.

AppleJackRP no longer has a repository-owned server entrypoint, Docker image,
hosted API, or Windows supervisor. Do not launch `sbox-server.exe` directly:
doing so skips Cellar's bridge document, graceful shutdown and restart policy.

## Windows one-click startup

From a Cellar checkout, run:

```powershell
.\scripts\install-applejack-shortcut.ps1
# Or choose the per-user config explicitly:
.\scripts\install-applejack-shortcut.ps1 -Config "$env:LOCALAPPDATA\AppleJackRP\cellar.toml"
```

This creates `AppleJackRP.lnk` on the desktop and starts the Cellar tray icon at
login. By default it creates a local development profile and adds
`-Development -Watch` to the desktop shortcut. The shortcut validates the config, syncs the AppleJackRP checkout
to the external runtime, checks both Cellar and AppleJackRP for updates, asks
before installing either update, starts Cellar, waits for its health endpoint,
then runs `cellar doctor`. The tray menu
opens the dashboard and provides start, stop, restart, status, and update
checks. Pass `-NoStartup` if the login tray shortcut is not wanted.

For a published lobby profile, pass `-Published -NoWatch` to
`install-applejack-shortcut.ps1`. An existing config is preserved, so switching
an already-installed desktop shortcut between local and published profiles
requires editing or replacing its config first. The installer places
development and published Windows profiles beside the per-user config so the
web UI can switch modes at runtime.

Cellar releases can also attach an AppleJackRP release bundle. It contains the
checked-out `.sbproj`, game source and assets, generated build identity, and
both gamemode changelogs. Configure the Cellar repository secret
`APPLEJACK_RELEASE_TOKEN` with read access to the private AppleJackRP repository
to enable that release asset. Publishing the compiled package to `sbox.game`
still requires the s&box editor's package workflow.

The launcher keeps update policy opt-in: a failed or declined update never
prevents an offline local start. An invalid config stops startup with a visible
diagnostic message, while a post-start `doctor` warning remains visible without
taking down a healthy Cellar process. Update application remains protected by
Cellar's empty-server and checksum checks.

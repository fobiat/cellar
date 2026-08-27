# AppleJackRP configuration

Cellar is the server manager for AppleJackRP. The AppleJackRP repository keeps
the game project and gameplay code; Cellar owns the dedicated-server process,
the local MariaDB process, the document bridge, the operator dashboard and
restart policy.

Start with the checked-in [AppleJackRP config](../configs/applejackrp.toml).
Copy it to the Cellar config path and change only the paths that differ on the
host. The Windows installer uses `%ProgramData%\Cellar\cellar.toml`.

The config uses an external runtime copy of the local `.sbproj`, so the
dedicated server's s&box process does not write `.sbox` state into the
checkout. Sync the repository with
`tools\sync-cellar-runtime.ps1` before starting and after source changes. To
run a published package, remove `server.project` and set `server.game` plus
`server.map`. Keep `server.data_dir` pointed at the same package data directory
that contains `features.json` and `permissions.json`; Cellar writes
`hosting.json` there before launching the game.

Before the first run, make sure `CELLAR_DATABASE_URL` is available to the
Cellar process. If the managed MariaDB block is enabled, provision it once:

```powershell
cd C:\Users\Shadow\Desktop\Projects\AppleJackRP-sandbox
.\tools\sync-cellar-runtime.ps1
cellar mariadb provision --config "$env:ProgramData\Cellar\cellar.toml"
# Set CELLAR_DATABASE_URL to the URL printed by the provision command.
cellar run --config "$env:ProgramData\Cellar\cellar.toml"
```

With Cellar running, use a second shell for `cellar doctor --config
"$env:ProgramData\Cellar\cellar.toml"`. The doctor command checks the live
managed database as well as the server configuration.

The dashboard is at `http://127.0.0.1:8081`. Use `/readyz` for the game-server
readiness probe and `/healthz` for the Cellar process liveness probe.

The checked-in `configs/applejackrp-public.toml` profile runs the published
package with `thieves.rpdowntown3t`, so the s&box lobby can resolve the game and
map thumbnails. The local project profile is for development and does not have
published package branding. Switch profiles from Cellar's Configs tab, or copy
the public profile beside the active config before starting Cellar.

AppleJackRP no longer has a repository-owned server entrypoint, Docker image,
hosted API, or Windows supervisor. Do not launch `sbox-server.exe` directly:
doing so skips Cellar's bridge document, graceful shutdown and restart policy.

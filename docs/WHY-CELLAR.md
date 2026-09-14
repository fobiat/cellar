# Why Cellar?

Running an s&box server is easy to start and hard to operate well. Cellar gives
the server a dependable home: it keeps the real console alive, knows when the
gamemode is actually ready, and gives operators one safe view of what is
happening.

## The short version

Cellar turns an s&box server from a process into an operated service.

- A live WebUI, TUI, CLI, and WebSocket API show the same state.
- The server runs in a real pseudo-terminal, so commands work as they do in
  the s&box console instead of disappearing into redirected output.
- Readiness comes from the gamemode's own signal, not from an open port that
  may exist while the map or code is still loading.
- Gamemode profiles provide command discovery, safe shortcuts, map checks, and
  startup diagnostics without hardcoding one game's implementation into Cellar.
- Operators can inspect the gamemode database and, when explicitly enabled,
  apply controlled data or schema changes with confirmation and an audit trail.
- Database backups and bridge-document persistence snapshots can be verified,
  exported, restored, and rolled back from the same operator surface.
- Crash-loop detection, graceful shutdown, health probes, update policy, and
  optional notifications make unattended hosting observable.
- Native Linux, native Windows, explicit Wine fallback, Docker, Kubernetes,
  LAN, Tailscale, HTTPS reverse proxies, and mobile browsers are supported by
  the same configuration model.

## Cellar compared

| Approach | What it does well | What Cellar adds |
| --- | --- | --- |
| Run s&box directly | Quick local testing | Readiness, restart policy, live operator access, database workflows, probes, and recovery evidence |
| systemd, Docker, or Kubernetes alone | Starts and restarts a process | s&box pseudo-terminal handling, gamemode-aware health, command discovery, database controls, and a purpose-built dashboard |
| A game-specific admin panel | Deep controls for one gamemode | A neutral control plane that works across gamemodes and preserves the game's own profile and schema ownership |
| Cellar | Process, console, health, data, and operator workflows in one place | The operational context that general-purpose process managers and isolated game panels leave separate |

Cellar does not replace the game. The gamemode still owns gameplay, package
publication, migrations, and the meaning of its data. That boundary keeps the
manager reusable and makes destructive controls deliberate.

## Why operators notice the difference

When a server is unhealthy, the question is not only “is the process running?”
Cellar can answer which stage it reached, whether the console is live, whether
the map loaded, whether the database and bridge are connected, what commands
the gamemode exposed, what changed, and whether the last backup can actually be
restored.

That shortens the path from a player report to a verified fix. It also gives a
small team a repeatable way to run a serious server without building a custom
control plane for every gamemode.

## Try it

```sh
cellar doctor
cellar run
cellar tui
```

Open the WebUI from the URL Cellar prints. Start with the AppleJack Framework
reference config or the generic Facepunch Sandbox profile, then add a profile
for your own gamemode.

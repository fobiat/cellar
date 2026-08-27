# Cellar documentation

An open-source dedicated server runner and manager for s&box.

## Start here

| | |
| --- | --- |
| **[Quickstart](QUICKSTART.md)** | A server running in about ten minutes, on Windows or Linux. Start here. |
| **[Installation](INSTALLATION.md)** | Every install path: installers, Docker, Kubernetes, from source, upgrading, uninstalling. |
| **[Configuration](CONFIGURATION.md)** | Every `cellar.toml` key, its default, and what it does. |
| **[CLI reference](CLI.md)** | Every command and flag, with worked examples. |
| **[MCP integration](MCP.md)** | Read-only MCP tools, authenticated command access, and stdio client usage. |

## Going further

| | |
| --- | --- |
| **[Game database](GAME_DATABASE.md)** | Hosted database access, schema ownership, and the read-only operator browser. |
| **[Facepunch Sandbox](FACEPUNCH-SANDBOX.md)** | A published `facepunch.sandbox` profile for testing a second gamemode. |
| **[Operations](OPERATIONS.md)** | Running it for real: health probes, graceful shutdown, webhooks, updates, backups. |
| **[Troubleshooting](TROUBLESHOOTING.md)** | Symptoms, causes and fixes, including the ones that look like something else. |
| **[Architecture](ARCHITECTURE.md)** | How it is built and why, including the engine findings the design rests on. |

## The one thing to know before anything else

The s&box dedicated server **will not accept commands over a pipe.** It builds
its console only when its output is a terminal, and reads that console with
`Console.ReadKey()`, which throws on a redirected stream. Cellar runs the server
on a pseudo-terminal for exactly this reason.

The practical consequence for you: under Docker or Kubernetes you need
`tty: true` and `stdin: true`, or the console is silently unavailable. See
[Architecture](ARCHITECTURE.md#the-console-needs-a-terminal-not-a-pipe).

## Conventions in these docs

Commands are shown for Linux. On Windows the binary is `cellar.exe` and paths
use backslashes, which in TOML means single quotes:

```toml
executable = 'C:\sbox\sbox-server.exe'
```

Anything marked **unproven** has not been run against a live s&box server. It is
marked rather than omitted, because a claim you cannot check is worse than a gap
you can see. [Troubleshooting](TROUBLESHOOTING.md#what-has-not-been-proven) lists
them in one place.

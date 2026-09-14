# AppleJackRP integration

AppleJackRP is an optional Cellar integration. AppleJackRP owns its game code,
assets, package publication, schema, and gameplay policy. Cellar owns the
dedicated-server process, pseudo-terminal, readiness probe, operator console,
restart policy, and dashboard.

This page records the integration contract in Cellar's documentation. It does
not make AppleJackRP a Cellar default, and Cellar remains usable without the
AppleJackRP repository.

## Profile

Keep the AppleJackRP profile beside the Cellar config or in the AppleJackRP
checkout. A minimal profile looks like this:

```toml
name = "AppleJackRP"
ready_pattern = "Lobby created - session is joinable"
convar_prefix = "applejack"

[[command]]
label = "List features"
command = "applejack_features"
group = "features"

[[command]]
label = "List settings"
command = "applejack_settings"
group = "features"
```

The prefix is the only AppleJackRP-specific value Cellar needs for automatic
command discovery. Once the server is ready, Cellar runs `find applejack`,
keeps only valid `applejack_*` command names, and adds them to the command
palette. Discovered commands require confirmation. Declare a command when it
has a useful label or a known safety policy.

The profile may also declare AppleJackRP map package idents and source checks.
Those checks are relative to the project directory and cannot read outside it.
They are optional diagnostics, not Cellar's interpretation of the game.

## Local and published servers

Use `server.project` for a local `.sbproj` and `server.game` for a published
package. Give each mode the data directory that the running package reads.
Do not share one data directory between local and published modes unless the
game explicitly supports that arrangement.

On Linux, choose `launcher = "wine"` for a Windows dedicated-server binary and
give each concurrent instance its own Wine prefix. On Windows, use the native
launcher for a Windows binary. The platform choice belongs in the deployment
config, not in Cellar's AppleJackRP integration code.

## Database and bridge

The AppleJackRP game owns its schema and migrations. Set the database URL in
`CELLAR_DATABASE_URL` or a protected URL file. Keep credentials out of
`cellar.toml`, scripts, shortcuts, and issue reports.

If AppleJackRP uses Cellar's hosted-document bridge, configure the bridge data
directory and shared secret according to the main [Bridge](../BRIDGE.md)
guide. Cellar writes the bridge document and serves the authenticated
protocol. AppleJackRP decides which game documents exist and how they are
interpreted.

## Operator workflow

1. Copy the profile and set the executable, project or game, map, data path,
   and log path for the host.
2. Run `cellar doctor --config <path>`.
3. Start `cellar run --config <path>` in a terminal or service with a real pty.
4. Confirm `/healthz`, `/readyz`, and the dashboard status.
5. Use the gamemode tab for declared and discovered commands. Confirm
   destructive actions before sending them.

The AppleJackRP repository remains the source of truth for game updates. Do
not add an AppleJackRP build, editor publish, or package token to Cellar's
public release workflow.

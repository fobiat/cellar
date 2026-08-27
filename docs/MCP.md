# MCP integration

Cellar exposes a small Model Context Protocol server over stdio. An MCP host
can launch it as a child process and receive live, structured data from the
running Cellar instance.

The server provides these tools:

- `cellar_status`: server, database, access, anti-cheat, and address state.
- `cellar_logs`: persistent log search by text, tag, level, and limit.
- `cellar_resources`: process, host, and network telemetry.
- `cellar_addresses`: local, game, query, bridge, and Tailscale addresses.
- `cellar_versions`: installed and remote versions plus build drift.
- `cellar_configs`: sanitized available profile names and metadata.
- `cellar_command`: an explicitly enabled operator command path.

## Expose Cellar to an MCP host

Start Cellar with the web API enabled and an external API bearer token. The
token is read from the environment and is never returned by the MCP server:

```powershell
$env:CELLAR_API_TOKEN = 'use-a-long-random-token'
cellar mcp serve --url http://127.0.0.1:8081
```

Configure the MCP host to launch the same command. For example, a local host
configuration can use:

```json
{
  "mcpServers": {
    "cellar": {
      "command": "cellar",
      "args": ["mcp", "serve", "--url", "http://127.0.0.1:8081"],
      "env": {
        "CELLAR_API_TOKEN": "set-this-in-the-host-secret-store"
      }
    }
  }
}
```

Read tools call Cellar's existing read-only `/api/v1` endpoints. Without
`CELLAR_API_TOKEN`, they return an actionable tool error and do not fall back
to the browser session or database credentials.

## Command tool

`cellar_command` reaches the existing operator-authenticated `/api/exec`
endpoint. It is disabled by default. To enable it, set both variables in the
MCP process environment:

```powershell
$env:CELLAR_MCP_ENABLE_COMMAND = '1'
$env:CELLAR_WEB_PASSWORD = 'the-operator-password'
cellar mcp serve --url http://127.0.0.1:8081
```

The web password is used only to obtain Cellar's normal session cookie. If the
web UI has no password and is loopback-only, the existing local policy may
accept the command without `CELLAR_WEB_PASSWORD`. Exposing the command tool on
a reachable web bind without password authentication is refused by Cellar's
configuration validation.

Cellar still rejects empty or multi-line commands and records successful
commands through the same audit path as the UI. MCP does not receive a second
privileged command channel.

## Use Cellar as an MCP client

The CLI can inspect or call another stdio MCP server:

```powershell
cellar mcp tools node --arg server.js
cellar mcp call node --arg server.js --tool some_tool --input '{"key":"value"}'
```

The client uses the standard MCP initialization and tool discovery flow. It
does not copy environment variables or credentials into the child process.
Arguments are passed exactly as supplied by the operator.

## Security notes

Keep MCP stdio processes under the MCP host's normal process policy. The
Cellar MCP server does not expose database URLs, passwords, API tokens, local
profile paths, or arbitrary files. MCP read access is separate from the
operator session, while command access remains behind the existing web auth
boundary plus the explicit MCP opt-in.

# CLI reference

Every command, every flag. Generated against the shipped binary, not from
memory: `cellar <command> --help` is the authority if these ever disagree.

## Global options

Available on every command.

| Flag | Default | Meaning |
| --- | --- | --- |
| `-c`, `--config <PATH>` | `cellar.toml` | Path to the config file. |
| `--log <FILTER>` | `info` | Log filter, for example `debug` or `cellar_runtime=debug`. Also `CELLAR_LOG`. |
| `--instance <ID>` | the primary | Which supervised server to act on, for a config declaring several. Applies to `exec`, `settings` and `doc`. An id the config does not declare is refused with the real ids listed, rather than quietly reaching the primary. |
| `-h`, `--help` | | Help for the command. `--help` is longer than `-h` where the two differ. |
| `-V`, `--version` | | Cellar's own version. Top level only. |

## Which commands need a running server

Three groups, and mixing them up is the most common confusion:

| Group | Commands | Needs |
| --- | --- | --- |
| **Standalone** | `run`, `doctor`, `config`, `hash-password`, `self-update`, `mariadb *` | Nothing running. |
| **Database** | `db *`, `doc *` | `CELLAR_DATABASE_URL`. Not a running server. |
| **Live server** | `settings *`, and `version`/`changelog` for live figures | A `cellar run` with `web.enabled = true`. |

Live-server commands talk to `cellar run` over its web API on `web.bind`. If
that server's web UI has a password, set `CELLAR_WEB_PASSWORD` (the plain
password, not the hash) for these commands to authenticate.

---

## `cellar run`

Supervise the server in the foreground. This is the container entrypoint.

```
cellar run [--tui]
```

| Flag | Meaning |
| --- | --- |
| `--tui` | Also draw the terminal dashboard. Needs a real terminal. |

Starts the server on a pseudo-terminal, tails its log, tracks players, serves
the bridge and the web UI, and stops it gracefully on `SIGTERM` or `Ctrl-C`.

Runs until the server exits and the restart policy says to stop, or until it is
asked to stop. See [`supervisor.restart`](CONFIGURATION.md#supervisor).

---

## `cellar doctor`

Check the config and the environment without starting anything.

```
cellar doctor
```

Verifies the executable exists and is runnable, the project file parses, Wine is
present when `launcher = "wine"`, the log path is writable, the database is
reachable when enabled, and the bind addresses are free. Run it first, every
time; it turns a confusing startup failure into a sentence.

---

## `cellar config`

Print what the config resolves to, with every secret redacted.

```
cellar config
```

Shows the file merged with environment overrides and defaults filled in, which
is the thing that actually takes effect. Secrets print as `***` and cannot be
recovered from this output, so it is safe to paste into an issue.

---

## `cellar version`

Show installed and available versions.

```
cellar version [--json]
```

Reports Cellar's own version, the gamemode's build stamp, the git remote's HEAD,
and the installed Steam build id, side by side. The stale-stamp problem in
AppleJackRP was found exactly this way: the stamp said one commit, the remote
said another.

---

## `cellar changelog`

Show the gamemode's changelog.

```
cellar changelog [--limit N] [--json]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--limit <N>` | `3` | How many releases to show. |

---

## `cellar update`

Check for gamemode and engine updates, and optionally take them.

```
cellar update [--check] [--now] [--force]
```

| Flag | Meaning |
| --- | --- |
| `--check` | Report only, whatever the configured policy says. |
| `--now` | Apply now, ignoring the maintenance window. Still refuses a dirty checkout, and still refuses a populated server unless `--force`. |
| `--force` | Apply even with players connected. Says what it is doing first. |

The default policy is `notify`: taking an update is opt-in. The updater will not
restart a server with people on it unless you make it, which is the whole reason
`only_when_empty` defaults to true. See [The updater](OPERATIONS.md#updates).

---

## `cellar self-update`

Update Cellar itself from the published releases.

```
cellar self-update [--check]
```

| Flag | Meaning |
| --- | --- |
| `--check` | Report what is available without installing it. |

Downloads the release asset for this platform, verifies its published SHA-256,
and installs it by renaming the running binary aside rather than writing over it,
so a failed update leaves a working Cellar in place.

Public releases need no token. Set `CELLAR_GITHUB_TOKEN` or `GITHUB_TOKEN` when
using a private fork.

---

## `cellar db`

Database maintenance. Needs `CELLAR_DATABASE_URL`, not a running server.

```
cellar db migrate     # apply pending migrations
cellar db status      # show the schema and row counts
cellar db prune       # delete events older than the configured retention
cellar db backup      # write a timestamped dump and prune old ones
cellar db backups     # list the dumps in the backup directory, newest first
cellar db restore     # apply a dump back over the database
```

`migrate` is safe to run repeatedly; it applies only what is pending. It is
**not** run by `cellar run` while `database.schema_owner = "gamemode"`, which is
the default, so a first run against a fresh database needs it explicitly.

`prune` honours `database.event_retention_days`. It deletes operational events
only. It never touches `aj_document` or its revision history.

`backup` and `backups` use `backup.directory`, falling back to
`mariadb.data_dir/backups`. Dumps are named `cellar-<unix seconds>.sql` and
`backup.retain` bounds how many are kept.

### `cellar db restore [dump] [--yes]`

Applies a dump back over the database, replacing every table the dump carries.
With no argument it takes the newest dump in the backup directory.

**Stop the server first.** The gamemode writes through the bridge continuously,
and a write landing mid-restore lands in a table that is about to be dropped.
The command refuses to run while a Cellar is answering on `web.bind`, and the
`POST /api/db/restore` endpoint stops the supervised server itself and says so
in its reply. Neither starts it again: whoever restored a database should look
at it before players reach it.

Two other refusals, both before anything reaches the database:

- The file must start like something `mariadb-dump` wrote. Without that check a
  wrong path is piped into the database as SQL.
- Without `--yes` it asks for the database name to be typed back.

What restore does not undo: a dump carries `DROP TABLE IF EXISTS` before each
`CREATE TABLE`, so tables the dump does not carry are left as they are. Applying
an older dump over a newer schema can leave a table behind that the dump knows
nothing about.

---

## `cellar mariadb`

Only does anything with `[mariadb]` `managed = true`. Separate from `db`
above: that operates on whatever `database.url` already points at, local or
remote, and needs no `[mariadb]` section to do it.

```
cellar mariadb provision   # download, initialize, and (re-)create the database, user and password
cellar mariadb status      # report install/data-directory state without starting anything
```

`provision` is safe to run repeatedly, including to recover a lost password:
installing and initializing are skipped once already done, but the database,
user and password are always (re-)applied, and a fresh
`CELLAR_DATABASE_URL` is printed every time. There is deliberately no
`start`/`stop`/`restart` here; `cellar run` is the only thing that supervises
the instance, the same way it is the only thing that starts the game server.

---

## `cellar doc`

Read and write bridge documents from the command line. Reads the database
directly, so it works whether or not a server is running.

```
cellar doc ls [PREFIX]      # list documents, optionally filtered
cellar doc get <KEY>        # print one document
cellar doc put <KEY> <FILE> # write one, or `-` for stdin
cellar doc history <KEY>    # every revision, and who wrote it
```

Worked examples:

```sh
cellar doc ls characters/                       # every character
cellar doc get characters/76561198000000000.json
cellar doc get features.json > features.backup.json
cellar doc put features.json features.backup.json
echo '{"version":1}' | cellar doc put test.json -
cellar doc history characters/76561198000000000.json
```

`history` answers "who overwrote my character", which is the question the
revision table exists for.

Keys are validated, not sanitised: `[a-z0-9._-]` with `/` separators, at most 128
characters, no `.` or `..` segment, no reserved device name. An illegal key is
refused. See [The bridge](BRIDGE.md#document-keys).

---

## `cellar mcp`

Expose Cellar to an MCP host, or inspect and call another stdio MCP server.

```
cellar mcp serve [--url URL]
cellar mcp tools <COMMAND> [--arg ARG]...
cellar mcp call <COMMAND> --tool NAME [--input JSON] [--arg ARG]...
```

`serve` proxies the running Cellar API. Read tools require
`CELLAR_API_TOKEN`. The optional `cellar_command` tool additionally requires
`CELLAR_MCP_ENABLE_COMMAND=1` and authenticates through Cellar's existing web
operator session using `CELLAR_WEB_PASSWORD` when configured. See
[MCP integration](MCP.md) for the host configuration and the complete tool
list.

---

## `cellar settings`

Read and write the running server's configuration. Needs a `cellar run` with
`web.enabled = true`.

```
cellar settings dump [--overrides] [--yaml] [-o FILE] [--find PREFIX]
cellar settings diff <FILE>
cellar settings apply <FILE> [--dry-run]
cellar settings set <ID> <VALUE>
```

### `dump`

| Flag | Default | Meaning |
| --- | --- | --- |
| `--overrides` | off | Only what an operator changed away from the defaults. |
| `--yaml` | off | Write YAML instead of TOML. |
| `-o`, `--output <FILE>` | stdout | Write to a file. |
| `--find <PREFIX>` | `applejack` | Include the engine's own convars matching this prefix. |

`--overrides` is the shape worth committing. A full dump is mostly defaults and
mostly noise in a diff.

### `apply` and `diff`

| Flag | Meaning |
| --- | --- |
| `--dry-run` | Print the commands instead of sending them. `apply` only. |

Two behaviours that are deliberate:

- **A partial file sets what it names and nothing else.** It is not a request to
  reset everything it omits. The other reading turns a three-line file into a way
  to wipe a server's configuration.
- **The live server's catalogue decides what is writable, not the file.** A file
  claiming a `core` feature is toggleable is still refused. `boot` features are
  applied and flagged as needing a restart.

### A worked round trip

```sh
cellar settings dump --overrides -o server.toml   # capture
git add server.toml && git commit -m "server config"
cellar settings diff server.toml                  # what would change?
cellar settings apply server.toml --dry-run       # what would be sent?
cellar settings apply server.toml                 # send it
```

---

## `cellar exec`

Type a command into the running server's console and print what comes back.
Needs a running `cellar run` to talk to.

```
cellar exec status
cellar exec applejack_features
cellar exec --file boot.cfg --keep-going
cellar exec --json status
```

The console runs at full engine privilege, which is why native commands like
`status`, `kick` and `quit` work there and are refused to gamemode code. Every
call is audited the same way the web UI's console is: operator, command and
reply land in `srv_command`.

Quoting is optional, so `cellar exec status` and `cellar exec "status"` are the
same call. `--file` runs every non-blank, non-`#` line in order and stops at the
first failure unless `--keep-going` is given; the exit code still reports a
failure either way. `--json` emits one object per command rather than bare reply
lines.

A reply is bracketed by the console's own echo and status redraw rather than
waited out on a timer, so it comes back as soon as the server is done with it.

---

## `cellar hash-password`

Hash an operator password for the web UI.

```
cellar hash-password
```

Prompts without echoing, prints an argon2 hash. Put it in
`CELLAR_WEB_PASSWORD_HASH`. Required before `web.bind` may be a non-loopback
address.

The hash goes in the environment; the plain password goes in
`CELLAR_WEB_PASSWORD` only if you want the CLI's live-server commands to log in
by themselves.

---

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. For `run`, the server exited and the restart policy said stop. |
| `1` | The command failed. The reason is on stderr. |
| `2` | Bad usage: an unknown flag or a missing argument. |

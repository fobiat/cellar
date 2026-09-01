# Architecture

Why Cellar is shaped the way it is. Most of this is engine behaviour that is not
obvious and cost real time to establish, so it is written down once here rather
than restated at every site that depends on it.

---

## The console needs a terminal, not a pipe

The obvious design is "the supervisor owns the child's stdin and types commands
into it". **That design cannot work**, and the engine source says why.

`engine/Launcher/SboxServer/Launcher.cs`:

```csharp
// Redirecting console output (docker/wine/etc..) won't support this overlay, so don't bother with it
if ( !Console.IsOutputRedirected )
{
    console = new DedicatedServerConsole();
    console.Update();
}
```

If stdout is a pipe the console object is **never constructed**, so nothing ever
reads stdin. There is no alternate input path.

And when it *is* constructed, `ConsoleInput.Update` reads with
`Console.ReadKey()`, guarded by `Console.BufferWidth > 0`. `Console.ReadKey`
throws on a redirected stdin.

A **pseudo-terminal** satisfies both checks: a PTY is a terminal, so
`IsOutputRedirected` is false, `BufferWidth` is real, and `ReadKey` works. Cellar
spawns the server on one via `portable-pty`, which is ConPTY on Windows and
`forkpty` on Unix behind a single API. That is also most of the cross-platform
story.

This is why the existing Kubernetes deployment needs `stdin: true, tty: true`.
The manifest attributes it to Kubernetes wiring; the real reason is this check.

### What that buys

`DedicatedServerConsole.OnInput` dispatches through `ConVarSystem.Run( input )`
with `allowProtected: true`. That is **full engine privilege**: native `quit`,
`kick` and `status`, plus all 95 `applejack_*` commands, **with no gamemode
change at all**.

Gamemode C# cannot do this. `ConsoleSystem.Run` refuses anything `IsProtected`.

And because a console-typed `ConCmd` has no caller `Connection`, the gamemode's
`Reply()` helper falls back to `Log.Info`, which lands in the log file. So
command output is readable as well as writable.

There is no RCON in s&box. This is the substitute, and it is a better one.

---

## Two output channels, used for different things

| | PTY console | `logs/sbox-server.log` |
| --- | --- | --- |
| Timestamp | `hh:mm:ss`, 12 hour, no date | `yyyy/MM/dd HH:mm:ss.ffff`, dated |
| Logger name | truncated and padded to exactly 8 chars | complete |
| Separator | spaces | tabs |
| Extras | status bar, cursor movement, input echo | clean |
| Cellar uses it for | **writing commands**, command replies, frame timings | **all event parsing** |

Events are parsed from the **log file**, because the console truncates logger
names to 8 characters (collapsing distinct AppleJack categories) and carries no
date. The file format is tab-separated and unambiguous.

Rotation is handled: the engine sets `ArchiveOldFileOnStartup`,
`ArchiveAboveSize = 512MB`, `ArchiveEvery = Day`, and `KeepFileOpen = true`, so
under Wine the handle stays open and the tailer tolerates that rather than
assuming otherwise.

Both channels carry the same lines, so parsing both counts everything twice. The
log file wins; the console is held for two seconds at startup so the polling
tailer gets a turn, and only becomes the event source if the log file never
speaks at all.

---

## The status bar is two lines, and its clock is wrong

`DedicatedServerConsole.UpdateStatus` sets two separate status lines:

```
{name} ({players}/{max}) [{h:mm:ss}]                    Network {n}ms
Physics {n}ms, NavMesh {n}ms, Animation {n}ms            Update {n}ms
```

Line B has **no player counter and no clock**, so a parser anchored on the
counter rejects it outright. It is redrawn every half second, so that mistake
means the unparsed counter climbs forever while the frame timings never arrive.
Cellar parses the halves separately and merges them.

Those timings exist nowhere else. The status bar is the only place the engine
reports its own frame cost, which is why it is worth reading at all.

**The hour is wrong.** It is rendered as `{TotalHours:n0}`, which rounds rather
than truncating, while `mm\:ss` is exact. Forty minutes of uptime renders
`1:40:00`. The error is exactly `minutes >= 30`, so it is invertible, and Cellar
reports the true uptime from a bar the engine renders an hour ahead.

`crates/cellar-core/src/statusbar.rs` renders the bar the way the engine does,
padding and right-alignment included, so the tests are transcriptions of the
engine rather than impressions of it.

---

## Command replies are bracketed, not timed

`ConsoleInput.OnEnter` writes the echo **before** dispatching:

```csharp
Console.WriteLine( "> " + inputString );
...
OnInputText( strtext );      // -> ConVarSystem.Run
```

and `RedrawInputLine()` repaints the status block **after** the ConCmd returns.

So a reply is exactly the lines between `> {command}` and the next status line.
No timing guess is involved. A 3s timeout survives as a backstop for a console
that never echoes, but it is not the mechanism.

Measured against the fake server: **0.15s bracketed, 3.23s when the echo is
suppressed**, and the test fails in the second case rather than passing slowly.

Those markers appear only on the PTY, never in the log file, which is why
replies are collected from the console while events come from the file. Nothing
is double-counted.

The PTY also **echoes what is typed**, twice over: `Console.ReadKey()` is called
without `intercept: true`, so the runtime echoes each character, and then
`RedrawInputLine` reprints the accumulated buffer. Both are filtered.

---

## Event grammar

Join and leave, verbatim from `NetworkSystem.Handshake.cs` and
`NetworkSystem.Connections.cs`:

```
{name} [{steamid}] is connected
{name} [{steamid}] disconnected
Kicking {name} [{steamid}] - {reason}
```

**The SteamID is parsed by anchoring at the end, never by searching from the
left.** A Steam display name is chosen by the account holder and may contain
`[`, `]` and spaces, so a player called

```
Bob [76561198000000000] is connected
```

is legal, and produces a real join line containing a forged id. Searching from
the left finds the forgery; searching from the right finds the engine's own.
There is a test for exactly that name.

An unmatched line becomes `Event::Unparsed` and is counted, never dropped: a
parser that quietly stops matching is the failure mode worth engineering
against.

The grammar is pure functions over `&str` in one file, so when Facepunch changes
a log string, one file changes and its tests say what broke.

---

## Crates

Dependency direction is strictly one way. No cycles, and `cellar-core` compiles
alone so its tests run in milliseconds with no game server, no Wine and no MySQL.

```
cellar-core       pure. no I/O, no tokio.
                  config, events, grammar, statusbar, lifecycle,
                  doc_key, convar, secret, theme, ansi
cellar-runtime    process supervision: pty spawn, log tailing, metrics, shutdown
cellar-store      MySQL: migrations, documents, ops tables, the read-only browser
cellar-server     axum: bridge, health, websocket, embedded web UI
cellar-notify     Discord and generic webhooks, batched and rate-limit aware
cellar-tui        ratatui dashboard
cellar-update     version checking and self-update
cellar-mariadb    downloads, provisions and supervises a local MariaDB,
                  and takes and restores logical dumps
cellar-mcp        an MCP server over stdio, and a small MCP client
cellar-cli        the `cellar` binary
cellar-fake-server  a stand-in for sbox-server.exe, for tests
```

`core` depends on nothing of ours. `runtime`, `store`, `notify`, `mariadb`
depend on `core`. `server` and `tui` depend on those. `cli` depends on all.

The parts that are easy to get quietly wrong (what counts as a player joining,
when to stop restarting, which document keys are legal, what a secret must never
do) are all functions over data in `core`.

`cellar-mariadb` sits beside `cellar-runtime` rather than inside it: the game
server needs a pty (see above), `mariadbd` is a normal console process that
needs none of that machinery, and bolting it onto the crate that exists
specifically for the PTY quirk would blur why that crate exists. It also
never depends on `cellar-store`: the one-time bootstrap in `provision.rs`
shells out to the bundled `mariadb`/`mariadb-admin`/`mariadb-install-db`
client tools rather than adding a second `sqlx` connection path, so
`cellar-store` stays the only crate that speaks the MySQL wire protocol.

---

## Hosting MariaDB natively, not in a container

`[mariadb]` (`managed = true`) downloads an official MariaDB release,
initializes a data directory and supervises `mariadbd` as a child process of
`cellar run`, the same way Cellar already supervises the game server. The
obvious alternative, a Docker or Podman container, was deliberately not
built: Cellar's own reference deployment target for this feature is a Shadow
PC cloud-gaming Windows VM, which has no Docker installed, and getting one
there means enabling Hyper-V or WSL2, an elevated, disruptive system-settings
change that is frequently unsupported or blocked outright by a cloud-gaming
host's nested-virtualization policy. A bundled binary needs neither: it is
the same trust model Cellar already uses for the game server itself.

The listener binds `127.0.0.1` unconditionally, hardcoded in
`cellar-mariadb::supervisor`, not exposed as a `[mariadb]` config key at all.
Unlike `bridge.bind`/`web.bind`, there is no legitimate reason for this one to
be reachable off the host, so the stronger guarantee (no code path can set it
wrong) was chosen over the weaker one (validate against a bad value).

---

## Testing without a game

End-to-end testing would otherwise need Steam, Wine, a GSLT and a real s&box
build. So `cellar-fake-server` opens a PTY, emits real engine log lines, writes a
real status bar (rendered by the same `cellar-core` code Cellar parses with, so
the two cannot drift), accepts console input including the `> ` echo, implements
`quit`, and can be told to crash, hang or flood.

Every supervisor, TUI, web and webhook test runs against it, on any machine, in
seconds.

**348 tests**, including the adversarial player name, rotation mid-line, split
UTF-8, and the ported C# protocol expectations so both halves of the bridge are
provably talking about the same thing.

The gate, before every commit:

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets --workspace -- -D warnings
cargo fmt --all --check
```

---

## Engine findings, collected

Four things established from source that change how this is built:

1. **`-allowlocalhttp` is mandatory for the bridge.** Without it `Http.IsAllowed`
   blocks direct IP literals, private addresses and loopback on ports other than
   80, 443, 8080 and 8443. A cluster-internal bridge is a private address.
2. **`+maxplayers` does nothing.** No such convar, no such launch switch. The
   ceiling is `Metadata.MaxPlayers` in the `.sbproj`. Cellar deliberately does
   not pass the inert flag.
3. **Steam A2S reports zero players, always.** Nothing calls `BUpdateUserData`,
   so the count and the `A2S_PLAYER` list are always empty. A2S is a liveness
   check only.
4. **There is no SIGTERM handler anywhere in the engine.** Killing the process
   skips all nine shutdown steps, including `ConVarSystem.SaveAll()` and the
   Steam `LogOff()`. Every Kubernetes rollout does this today.

---

## Where the engine writes: two instances cannot share an install tree

Measured on 2026-09-01, against a real `sbox-server.exe` from app 1892930's
Windows depot under Wine, because this decides whether concurrent instances can
share one s&box install and every previous answer to it was inferred.

**Both `logs/` and `data/` follow the executable's own directory.** Not the
working directory, and not `FACEPUNCH_ENGINE`.

| Run | `FACEPUNCH_ENGINE` | Working directory | `logs/` landed | `data/` landed |
| --- | --- | --- | --- | --- |
| 1 | process env, scratch dir | install dir | install dir | install dir |
| 2 | Wine registry `HKCU\Environment`, scratch dir | install dir | install dir | install dir |
| 3 | unset | scratch dir | next to the exe | next to the exe |

So:

- **`FACEPUNCH_ENGINE` relocates nothing here.** `Logging.cs` reads it with
  `EnvironmentVariableTarget.User`, which on Windows is the registry rather than
  the process environment, so passing it as a child environment variable could
  never have worked. Setting it in the Wine registry did not work either. The
  fallback, `AppContext.BaseDirectory`, is what is actually in effect.
- **`server.working_dir` does not move the log or the data directory.** The
  engine resolved its mounts against the install directory from a working
  directory elsewhere. Whatever `working_dir` is for, it is not this.
- **`server.log_file` remains Cellar's read path only.** Nothing Cellar passes
  changes the engine's write path.

**Consequence: one install tree per instance.** A shared tree gives two servers
one `logs/sbox-server.log`, so both tailers parse both servers' lines and every
player join is counted twice, and one `data/<org>/<package>`, so they share
`hosting.json`, `features.json` and `permissions.json`. `Config::validate`
refuses both collisions rather than letting them happen quietly. A few gigabytes
per tree is the price, and there is no cheaper option, only an unmeasured belief
that there was one.

The `WINEPREFIX` finding is separate and still stands: every Wine process in one
prefix shares a `wineserver` that jointly holds their sockets, and
`wineserver -k` is prefix-scoped, so instances need a prefix each on Linux.

---

## Stated assumptions

1. **Bridge auth starts unverified** (`trusted`), justified by the process-tree
   trust boundary and written down rather than implied. Revisit if a Facepunch
   token introspection endpoint appears.
2. **One server's data, off-box**, answering the spec's open question Q1 as the
   simple reading, with a `scope` column so the other reading is a migration.
3. **The bridge answers `204`, never `409`**, until the gamemode can act on a
   conflict.
4. **The PTY console survives Wine.** Read from source, not observed. If it does
   not, the console channel is gone and gamemode telemetry becomes mandatory.

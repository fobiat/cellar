# Installation

The repository is **private**, so every download path needs a GitHub token. If
that changes, the token requirement disappears from all of these.

Get one with `gh auth token`, or create a fine-grained PAT with read access to
`fobiat/cellar`. Cellar reads `CELLAR_GITHUB_TOKEN` first, then `GITHUB_TOKEN`.

> Without a token, GitHub answers `404` to an anonymous caller rather than `403`.
> Cellar distinguishes "no releases exist" from "no releases are visible to you"
> in its error message, because those look identical from the outside.

---

## Windows

```powershell
$env:CELLAR_GITHUB_TOKEN = gh auth token
irm https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.ps1 | iex
```

Installs `cellar.exe` to `%LOCALAPPDATA%\Programs\Cellar` (or
`%ProgramFiles%\Cellar` when elevated, which needs administrator), adds it to
`PATH`, and writes a starting config to `%ProgramData%\Cellar` if none exists.

Per-user is the default and needs no administrator. For an all-users install
plus a registered Windows service, from an **elevated** PowerShell:

```powershell
.\install.ps1 -System -Service
```

`-Service` implies `-System`. A specific version: `.\install.ps1 -Version v0.1.1`.

Manual instead: download `cellar-x86_64-pc-windows.zip` from the
[releases page](https://github.com/fobiat/cellar/releases), unzip it anywhere,
and run `cellar.exe`. The zip carries `cellar.toml.example` with the Windows
paths already commented in.

## Linux

```sh
export CELLAR_GITHUB_TOKEN="$(gh auth token)"
curl -fsSL https://raw.githubusercontent.com/fobiat/cellar/main/scripts/install.sh | sh
```

Installs to `/usr/local/bin` when run as root, otherwise `~/.local/bin`. Config
goes to `/etc/cellar` or `$XDG_CONFIG_HOME/cellar` to match.

Options:

```sh
sh install.sh --version v0.1.1              # a specific release
sh install.sh --from-file ./cellar.tar.gz   # a file you already downloaded
sudo sh install.sh --system --service       # /usr/local/bin plus a systemd unit
```

`--service` implies `--system`, and `--system` needs root. The generated unit
sets `TimeoutStopSec=60`, because the engine installs no SIGTERM handler and
systemd's default would kill it before its own shutdown finished.

**x86_64 only.** No arm64 build is published, and the installer says so rather
than downloading something that will not run. The dedicated server is a Windows
x86_64 binary under Wine, so an arm64 host cannot run what Cellar supervises
anyway. Build the CLI from source if you want it there.

## Verify what you downloaded

Every release asset ships with a `.sha256` beside it.

```sh
sha256sum -c cellar-x86_64-unknown-linux.tar.gz.sha256
```

```powershell
Get-FileHash cellar-x86_64-pc-windows.zip -Algorithm SHA256
```

`cellar self-update` does this check itself and refuses a mismatch.

---

## From source

Needs a Rust toolchain (edition 2024, so 1.85 or newer).

```sh
git clone https://github.com/fobiat/cellar
cd cellar
cargo build --release
./target/release/cellar --version
```

The binary is standalone. Copy `target/release/cellar` anywhere on your `PATH`.

Cross-compiling to Windows from Linux, without a host toolchain, using the
shipped Docker recipe:

```sh
./scripts/cross-build-windows.sh
```

## Docker

A `Dockerfile` is at the repo root.

```sh
docker build -t cellar .
```

Running it needs a TTY, which is not optional:

```sh
docker run -it \
  -e CELLAR_DATABASE_URL='mysql://cellar:secret@db/cellar' \
  -v /path/to/sbox:/home/container/sbox \
  -v /path/to/cellar.toml:/etc/cellar/cellar.toml \
  -p 8081:8081 \
  cellar run -c /etc/cellar/cellar.toml
```

**`-it` is load-bearing.** Without a TTY the engine never builds its console and
commands silently do nothing. See
[Architecture](ARCHITECTURE.md#the-console-needs-a-terminal-not-a-pipe).

## Kubernetes

The pod spec needs three things that are easy to miss:

```yaml
spec:
  terminationGracePeriodSeconds: 45   # >= supervisor.graceful_timeout_seconds
  containers:
    - name: server
      stdin: true                      # both of these, or no console
      tty: true
      readinessProbe:
        httpGet: { path: /readyz, port: 8080 }
      livenessProbe:
        httpGet: { path: /healthz, port: 8080 }
```

Full manifest and the reasoning in [Operations](OPERATIONS.md#kubernetes).

---

## Upgrading

```sh
cellar self-update --check    # what is available
cellar self-update            # take it
```

It downloads the asset for this platform, verifies the published SHA-256, and
installs by renaming the running binary aside rather than overwriting it. A
failed update therefore leaves a working Cellar in place.

Re-running the installer script works too and is equivalent.

Upgrading Cellar does not touch the database. Schema changes are applied by
`cellar db migrate`, or automatically at startup when
`database.migrate_on_start` is true.

---

## Uninstalling

**Linux**:

```sh
rm -f /usr/local/bin/cellar ~/.local/bin/cellar
rm -rf /etc/cellar ~/.config/cellar
```

**Windows** (PowerShell):

```powershell
Remove-Item -Recurse "$env:LOCALAPPDATA\Cellar"
Remove-Item -Recurse "$env:ProgramData\Cellar"
```

Then remove the `PATH` entry the installer added.

Neither drops the database. To do that as well:

```sql
DROP DATABASE cellar;
```

Take a backup first if any characters live there. `aj_document` is the real data;
everything with an `srv_` prefix is operational and regenerable.

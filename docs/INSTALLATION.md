# Installation

Public releases need no GitHub token. Cellar reads `CELLAR_GITHUB_TOKEN` first,
then `GITHUB_TOKEN`, when you install from a private fork.

> Without a token, a private fork answers `404` to an anonymous caller rather
> than `403`. Cellar distinguishes "no releases exist" from "no releases are
> visible to you" in its error message.

---

## Windows

```powershell
$version = 'v0.1.10'
Invoke-WebRequest "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.ps1" -OutFile install-cellar.ps1
Get-Content .\install-cellar.ps1
.\install-cellar.ps1 -Version $version
Remove-Item .\install-cellar.ps1
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

Add `-Run` when the config and game server are already ready. The installer
will run `cellar doctor` and then replace itself with `cellar run`.

```powershell
.\install.ps1 -Run
```

Manual instead: download `cellar-x86_64-pc-windows.zip` from the
[releases page](https://github.com/fobiat/cellar/releases), unzip it anywhere,
and run `cellar.exe`. The zip carries `cellar.toml.example` with the Windows
paths already commented in.

### Hosting MariaDB locally

Cellar needs a MySQL/MariaDB it can reach at `CELLAR_DATABASE_URL` for the
database browser and operational state. If nothing is already running on this
machine (or the network), Cellar can host one itself instead: set
`[mariadb]` `managed = true`, `version` and `sha256` in `cellar.toml` (see
[Configuration](CONFIGURATION.md#mariadb)), then:

```powershell
cellar mariadb provision
```

This downloads MariaDB's official archive for `version`, verifies it against
the pinned `sha256`, unpacks it, initializes a data directory, and creates
the `cellar` database, user and a generated password, printing:

```
CELLAR_DATABASE_URL='mysql://cellar:...@127.0.0.1:33306/cellar'
```

Set that in your environment (for `-Service`, add it to the service's
environment) before running `cellar run`, which starts and supervises
`mariadbd` itself from then on, alongside the game server.

**Why not Docker.** This is the path built for machines that cannot run
Docker at all, a Shadow PC or similar cloud-gaming VM being the case that
motivated it: no Docker installed, and turning on Hyper-V/WSL2 to get one is
an elevated, disruptive change that nested-virtualization cloud hosts often
block outright. See [Architecture](ARCHITECTURE.md#hosting-mariadb-natively-not-in-a-container)
for the full reasoning. If Docker already works on your machine, running
MariaDB in a container you manage yourself and pointing `CELLAR_DATABASE_URL`
at it works exactly the same as any other externally-hosted database; `cellar
mariadb` is only for hosts where that is not an option.

## Linux

```sh
version=v0.1.10
curl -fsSLO "https://raw.githubusercontent.com/fobiat/cellar/$version/scripts/install.sh"
less install.sh
sh install.sh --version "$version"
rm install.sh
```

Installs to `/usr/local/bin` when run as root, otherwise `~/.local/bin`. Config
goes to `/etc/cellar` or `$XDG_CONFIG_HOME/cellar` to match.

Options:

```sh
sh install.sh --version v0.1.1              # a specific release
sh install.sh --from-file ./cellar.tar.gz   # a file you already downloaded
sh install.sh --run                         # install, doctor, and start Cellar
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

## Docker and Swarm

A `Dockerfile` is at the repo root. It builds the Cellar layer, while the final
server image supplies the s&box executable, Wine, and the gamemode package.
Ready-to-adapt Compose and Swarm settings are in
[`deploy/docker-compose.yml`](../deploy/docker-compose.yml).

```sh
docker build -t cellar-layer .
```

The image entrypoint creates `/etc/cellar/cellar.toml` from the bundled example
when needed, runs `cellar doctor`, and then starts `cellar run`:

```sh
docker run -it \
  -e CELLAR_DATABASE_URL='mysql://cellar:secret@db/cellar' \
  -e CELLAR_WEB_PASSWORD_HASH='from-cellar-hash-password' \
  -v /path/to/sbox:/home/container/sbox \
  -v /path/to/cellar.toml:/etc/cellar/cellar.toml \
  -p 8081:8081 \
  your-registry/your-sbox-server:latest
```

Use `CELLAR_CONFIG` to select another config path. Pass `doctor`, `config`, or
another CLI command after the image name to run that command without the
automatic `doctor` plus `run` sequence.

The container must keep `stdin_open: true` and `tty: true`. Cellar gives the
engine a pseudo-terminal so console commands and readiness detection continue
to work without an interactive operator. See
[Architecture](ARCHITECTURE.md#the-console-needs-a-terminal-not-a-pipe).

For Swarm, use the same Compose file with `docker stack deploy` after replacing
the image and adding the registry secret. Keep one replica, mount persistent
game data and logs, and store database, UI password, and API tokens as Docker
secrets or environment secrets.

## Kubernetes

[`deploy/kubernetes.yaml`](../deploy/kubernetes.yaml) is a complete starting
manifest. It includes the one-replica `Recreate` strategy, persistent volumes,
HTTP probes, UDP game/query ports, and a 60 second termination grace period.
Create `cellar-secrets` before applying it. The image must contain the Cellar
entrypoint, the s&box dedicated server, Wine, and the selected gamemode.

The pod spec needs these settings:

```yaml
spec:
  terminationGracePeriodSeconds: 60   # >= supervisor.graceful_timeout_seconds
  containers:
    - name: server
      stdin: true                      # keep the engine console available
      tty: true
      readinessProbe:
        httpGet: { path: /readyz, port: 8081 }
      livenessProbe:
        httpGet: { path: /healthz, port: 8081 }
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

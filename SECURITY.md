# Security policy

Cellar supervises a game server and exposes a web console that can run
privileged engine commands. Treat an exposed instance as an administrative
service.

## Reporting a vulnerability

Please report security issues privately through
[GitHub Security Advisories](https://github.com/fobiat/cellar/security/advisories/new).
Do not open a public issue or include credentials, database URLs, Steam tokens,
webhook URLs, player data, or private server addresses in a report.

Include the affected version or commit, deployment mode, a concise description,
reproduction steps that do not disclose real secrets, and the impact. If the
advisory form is unavailable, contact the repository owner through
<https://github.com/fobiat> and ask for a private reporting channel.

## Supported versions

The latest tagged release is supported. Older releases may receive fixes when
the issue also affects the current release.

## Operational baseline

- Keep the web UI on loopback or behind TLS and an authenticated reverse proxy.
- Use `web.auth = "password"` with `CELLAR_WEB_PASSWORD_HASH` for a reachable
  operator UI.
- Give the database browser a dedicated read-only database user when the
  gamemode's schema is hosted elsewhere.
- Keep `CELLAR_GITHUB_TOKEN`, `CELLAR_DATABASE_URL`, `CELLAR_GSLT`, webhook URLs,
  and password hashes outside tracked files.

# Security and privacy

Cellar is an operator tool. Its console can dispatch privileged engine
commands, its updater can replace the Cellar binary, and its persistence
features can write files selected by configuration. Treat a reachable Cellar
web UI as an administrative service.

## Trust boundaries

- The gamemode owns gameplay rules, database schemas, and the meaning of bridge
  documents. Cellar only stores and restores the bridge boundary when asked.
- The running game process is supervised by Cellar but is not trusted to
  authenticate web callers or validate operator intent.
- The browser and TUI use the authenticated operator session. The external API
  token is read-only and does not authorize console commands.
- Backups are verified JSON documents. Restore accepts only files in Cellar's
  configured persistence directory or its configured export directory.

## Operator baseline

Bind the web UI to loopback or put it behind TLS and an authenticated reverse
proxy. Configure `web.auth = "password"` for every operator UI, including
loopback development instances. Keep `CELLAR_SESSION`, `CELLAR_WEB_PASSWORD_HASH`, `CELLAR_DATABASE_URL`,
`CELLAR_GSLT`, updater tokens, and webhook URLs outside tracked files.

Cellar also enforces this at the request boundary: every listener that is not
loopback-bound requires an authenticated operator session, even if a stale or
invalid runtime state claims that authentication is disabled. A non-loopback
listener cannot fall back to local trusted access.

The browser uses same-origin checks for state-changing requests, strict
SameSite cookies, and an HttpOnly session. HTTPS deployments use a `__Host-`
cookie name with Secure enabled. Sessions expire after twelve idle hours and
seven days absolute. The tray and remote TUI accept a session through
`CELLAR_SESSION`; they do not write it to disk. Session cookies are bearer
credentials, so revoke the web session by restarting Cellar if one is exposed.

## Privacy

Cellar has no analytics or telemetry service. It records operational events,
operator commands, and audit data in the configured database so an owner can
review what happened. Logs and persistence snapshots can contain player names,
Steam IDs, chat, or gamemode data. Treat those files as private server data and
include only redacted samples in issues.

## Release controls

CI runs dependency review, `cargo deny`, `cargo audit`, secret scanning, the
build and test gate, and browser accessibility checks. Release artifacts carry
checksums, an SPDX SBOM, and GitHub build provenance attestation. GitHub
workflow actions are pinned to full commit IDs.

Report vulnerabilities through the private process in the repository
[`SECURITY.md`](../SECURITY.md). Do not open a public issue for a live
credential, an unpatched command execution path, or private server data.

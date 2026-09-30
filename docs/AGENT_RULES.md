# Contributor rules

Keep changes within the agreed issue and preserve concurrent work. UI and
backend changes use isolated branches and worktrees. Update user documentation
and the changelog with visible behaviour changes. Restore tests use disposable
databases and trusted fixture SQL only. Never inspect production credentials or
runtime state during verification.

Before landing code, run formatting, build, lint, tests and a secrets scan.
The Rust gate is `cargo fmt --all -- --check`,
`cargo build --workspace --all-targets`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test --workspace`.
UI changes also run `scripts/check-browser.sh`. Database tests must run against
a disposable server with native clients installed when their paths change;
skipped integration tests do not establish acceptance.

Use VPS CI by default. Changes to hosted runners require an explicit documented
reason. Do not trigger hosted CI as a substitute for local verification.
Commits use the maintainer's signing identity. Code reaches the default branch
only after independent review of the final source and a green final gate, using
signed fast-forward merges. Never force-push or add attribution trailers.
Public changes must pass a leak scan before publication.

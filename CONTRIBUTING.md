# Contributing to Cellar

Cellar is an open-source dedicated server manager for s&box. Contributions
should keep the supervisor, CLI, TUI, web UI, installers, and hosted database
integration usable on both Windows and Linux.

## Before opening an issue

Run `cellar doctor` for configuration failures and search existing issues first.
Never include credentials, database URLs, Steam tokens, webhook URLs, player
data, or private server addresses. Use the security reporting process for a
vulnerability.

## Development checks

From the repository root:

```sh
cargo fmt --all --check
cargo build --workspace --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --workspace
./scripts/check-docs.sh
```

The Windows ConPTY integration test needs an interactive desktop. If that test
hangs in a non-interactive session, run the remaining library tests with
`cargo test --workspace --exclude cellar-runtime --lib` and report the omitted
test clearly.

Keep game schema ownership in the gamemode. Cellar may inspect a hosted schema
and run read-only queries, but it must not invent gamemode tables or migrations.

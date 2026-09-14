## What changed

<!-- Describe the operator or player problem this solves. Link an issue with
     `Fixes #123` when the change closes one. -->

## Verification

- [ ] `cargo fmt --all --check`
- [ ] `cargo build --workspace --all-targets`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `./scripts/check-docs.sh` and any relevant browser or integration checks
- [ ] I did not include passwords, tokens, database URLs, player data, or private host details

## Risk and rollout

<!-- Note migrations, config changes, restart requirements, security impact,
     and any platform or deployment path that still needs verification. -->

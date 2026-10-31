## What changes and why

## Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes, also with `--features tester`
- [ ] `cargo test` passes, also with `--features tester`
- [ ] Live tests pass against the local test server (`MUMBOT_LIVE=1 cargo test --features tester -- --ignored --test-threads=1`)
- [ ] Docs (README, `docs/`) and `CHANGELOG.md` are updated

# Contributing

Keep changes focused and explain their user-visible effect in a pull request.
Preserve compatibility with published commands, or document breaking changes
and migration steps explicitly. Never commit tokens, proxy databases, local
environment files, or build artifacts.

Before submitting a Rust change, run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --bins --locked
```

Tests must use local fixtures and temporary state. Do not change host routing,
call real providers for test generations, or rely on external proxy services.
Add regression tests for fixes, especially credential handling and HTTP 429.
Rate limits must not trigger replay, proxy rotation, or WARP changes.

CI validates Rust on Linux; the release workflow also tests and packages the
five supported targets on relevant pull requests. Pull requests and manual
workflow runs never publish a GitHub Release. Publishing requires a version tag.

The `proxy-hunt` notice and `reset-opencode` launcher are intentionally retained
for compatibility. `skills/oplire` is optional guidance, not a runtime dependency.

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

CI validates Rust on Linux for pull requests and pushes to `main`; documentation-only
changes skip it. Tests compile all executable targets, so CI does not repeat a
release build. Feature-branch pushes do not launch a second CI alongside the PR.
Cargo's registry and debug build are cached by platform and dependency manifest.
The release workflow tests and packages the five supported targets only for
version tags or an explicitly requested manual run. Manual runs do not publish
a GitHub Release. Publishing requires a version tag.

Releases provide five complete platform archives. Individual executable assets
are compatibility downloads used by already-published `install --latest`
clients, not additional builds. Do not remove them without an installer
migration plan.

The `proxy-hunt` notice and `reset-opencode` launcher are intentionally retained
for compatibility. `skills/oplire` is optional guidance, not a runtime dependency.

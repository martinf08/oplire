# oplire

`oplire` combines a small Anthropic-compatible API bridge for an
OpenAI/OpenCode-compatible chat API, a health-check CLI, and optional Linux
utilities for manually managing authorized proxies.

**An HTTP 429 or quota response never triggers an IP/proxy rotation, a WARP
reset, or a retry.** The bridge forwards the upstream status, response body,
and `Retry-After` header. Honor the indicated delay and the provider's terms.

## Features

- `POST /v1/messages` converts text-only Anthropic requests to upstream
  `POST /v1/chat/completions` requests.
- JSON responses and text SSE streams, model-list endpoints, and a local health
  endpoint.
- Images and Anthropic tools are rejected explicitly instead of being silently
  dropped.
- CLI commands: `oplire proxy`, `oplire check`, and `oplire models`.
- `bin/reset-opencode` remains as a compatibility launcher. It starts OpenCode
  without changing networking or sending a test generation. `--check URL` runs
  an explicit endpoint check first.
- `bin/proxy-rotate` retains manual management for user-configured, authorized
  proxies. Its watcher reports 429s without switching proxies. Automatic
  changes are limited to connectivity failures.
- `bin/proxy-hunt` is disabled; it does not collect or test public proxy lists.

The bridge binds to `127.0.0.1:8080` by default and targets
`http://127.0.0.1:3000`. It is restricted to loopback. For remote access, place
an authenticated reverse proxy in front of it and apply appropriate network
controls.

## Build and test

Rust stable and Cargo are required. On Debian/Ubuntu, `build-essential` may be
needed by native dependencies.

```sh
cargo fmt --all -- --check
cargo test --locked
cargo build --release --locked
./target/release/oplire --help
```

Tests use local HTTP servers. They verify, among other things, that an upstream
429 and its `Retry-After` header are forwarded without a retry.

## Run the bridge

The upstream must expose an OpenAI-compatible API, including
`POST /v1/chat/completions`.

```sh
OPLIRE_UPSTREAM_URL=http://127.0.0.1:3000 \
OPLIRE_LISTEN=127.0.0.1:8080 \
./target/release/oplire proxy
```

If the upstream requires a bearer token, set `OPLIRE_API_KEY` in the process
environment; do not put it in the repository or command history. Equivalent
CLI usage:

```sh
oplire proxy --listen 127.0.0.1:8080 --upstream http://127.0.0.1:3000
oplire check --url http://127.0.0.1:3000/health --attempts 3
oplire models
```

`oplire check` retries only bounded network errors and transient server errors.
It stops immediately on HTTP 429 and displays `Retry-After` when present.

To use the bridge with an Anthropic-compatible client, set its endpoint to
`http://127.0.0.1:8080` and choose a model provided by the upstream. Oplire
does not provide accounts or free provider access.

## Legacy Linux utilities

`install.sh` installs the Rust binary with Cargo and the Linux scripts. System
service installation is opt-in:

```sh
sudo ./install.sh                 # binary and scripts; no boot service
sudo ./install.sh --with-service  # opt in to OpenRC/systemd watcher
sudo ./install.sh --no-binary     # scripts only; do not install the bridge
sudo ./install.sh --dry-run
```

The legacy proxy pool is only for proxies you are authorized to use and
legitimate networking purposes. To configure one manually:

```sh
proxy-rotate init
proxy-rotate add 'http://user:pass@proxy.example:8080'
proxy-rotate list
proxy-rotate use ID
proxy-rotate status
```

`proxy-rotate 429` is informational and has no network effect. The watcher
does not switch proxies on 429; it can report a limit in its log. The manual
`proxy-rotate rotate` command must not be used to evade limits or quotas. Public
proxies are untrusted and should not carry credentials or sensitive data.

`reset-opencode-default` is a separate legacy utility for manually disabling
WARP. The bridge and launcher never call it, and it is not part of 429 handling.
Review its options before use; `--purge` deletes system files after making a
backup.

## Legacy pool configuration

Paths can be configured with `OPLIRE_STATE_DIR`, `OPLIRE_DB`,
`OPLIRE_ENV_FILE`, `OPLIRE_PROFILE_FILE`, `OPLIRE_RUN_DIR`, and
`OPENCODE_LOG`. The proxy environment file may contain credentials: keep it
private and out of version control.

## License and attribution

Distributed under the MIT License. Some API transformations and bridge
structure are adapted from `BerkeOruc/oplire` (copyright Berke Oruc); see
`LICENSE` for required notices.

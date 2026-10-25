# Oplire

Oplire is a small local API bridge and diagnostics CLI. It lets Anthropic-compatible
clients use an OpenAI-compatible chat API and provides optional Linux utilities
for manually managing authorized proxies.

> **HTTP 429 and quota safety:** Oplire never retries a rate-limited request,
> rotates an IP address or proxy, or resets WARP in response to HTTP 429. The
> bridge forwards the upstream status, response body, and `Retry-After` header.
> Follow the provider's indicated wait time and terms.

## Features

- `POST /v1/messages` translates text-only Anthropic requests to
  `POST /v1/chat/completions`.
- Supports JSON responses and server-sent event (SSE) streams.
- Provides model-list and local health endpoints.
- Rejects images and tool use explicitly rather than silently dropping them.
- Includes the `oplire proxy`, `oplire check`, and `oplire models` CLI commands.
- Binds to `127.0.0.1:8080` by default and targets
  `http://127.0.0.1:3000` by default.

The bridge is restricted to loopback. For remote access, put an authenticated
reverse proxy in front of it and apply appropriate network controls.

## Requirements

- Rust stable and Cargo.
- Python 3 to run the legacy-script tests.
- On Debian or Ubuntu, `build-essential` may be needed by native dependencies.

## Build and test

```sh
cargo fmt --all -- --check
cargo test --locked
cargo build --release --locked
./target/release/oplire --help
python3 -m unittest discover -s tests -v
```

The tests use local HTTP servers. They verify that HTTP 429 and `Retry-After`
are forwarded without replaying the request.

## Run the bridge

The upstream must expose an OpenAI-compatible API, including
`POST /v1/chat/completions`.

```sh
OPLIRE_UPSTREAM_URL=http://127.0.0.1:3000 \
OPLIRE_LISTEN=127.0.0.1:8080 \
./target/release/oplire proxy
```

If the upstream requires a bearer token, provide it through the process
environment as `OPLIRE_API_KEY`. Do not commit credentials or put them in shell
history.

Equivalent CLI commands:

```sh
oplire proxy --listen 127.0.0.1:8080 --upstream http://127.0.0.1:3000
oplire check --url http://127.0.0.1:3000/health --attempts 3
oplire models
```

The bridge exposes `GET /health`, `GET /v1/models`, and
`GET /v1/models/{model_id}` in addition to `POST /v1/messages`.

`oplire check` retries only bounded network errors and transient server errors.
It stops immediately on HTTP 429 and reports `Retry-After` when present.

To use the bridge, configure an Anthropic-compatible client with the endpoint
`http://127.0.0.1:8080` and a model supported by the upstream. Oplire does not
provide accounts or free provider access.

## Install

`install.sh` installs the Rust binary and Linux scripts. System-service
installation is opt-in:

```sh
sudo ./install.sh                 # Install the binary and scripts; no boot service
sudo ./install.sh --with-service  # Also enable the OpenRC/systemd watcher
sudo ./install.sh --no-binary     # Install scripts only
sudo ./install.sh --dry-run       # Preview the requested actions
```

## Legacy Linux utilities

The legacy proxy pool is for proxies you are authorized to use and legitimate
networking purposes only. Configure proxies manually:

```sh
proxy-rotate init
proxy-rotate add 'http://user:pass@proxy.example:8080'
proxy-rotate list
proxy-rotate use ID
proxy-rotate status
```

`proxy-rotate 429` is informational and has no network effect. The watcher can
report a limit in its log but never switches proxies on HTTP 429. The manual
`proxy-rotate rotate` command must not be used to evade provider limits or
quotas. Public proxies are untrusted; do not send them credentials or sensitive
data.

`bin/proxy-hunt` is disabled and does not collect or test public proxy lists.
`bin/reset-opencode` is a compatibility launcher that starts OpenCode without
changing networking or sending a test generation. `reset-opencode-default` is
a separate legacy WARP administration utility; it is not called by the bridge
or by HTTP 429 handling. Review its options before use. Its `--purge` option
deletes system files after making a backup.

## Legacy pool configuration

The legacy utilities support `OPLIRE_STATE_DIR`, `OPLIRE_DB`,
`OPLIRE_ENV_FILE`, `OPLIRE_PROFILE_FILE`, `OPLIRE_RUN_DIR`, and `OPENCODE_LOG`.
The proxy environment file may contain credentials; keep it private and out of
version control.

## License and attribution

Oplire is distributed under the MIT License. Some API transformations and
bridge structure are adapted from `BerkeOruc/oplire`; see `LICENSE` for the
required copyright and license notices.

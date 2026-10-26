# Oplire

Oplire is a Rust toolkit for a local Anthropic-compatible API bridge, endpoint
diagnostics, and explicitly managed network utilities. HTTP 429 and quota
responses never trigger a proxy or WARP change, and the bridge never replays a
rate-limited request.

## Release downloads

The release workflow publishes prebuilt archives to the [GitHub Releases page](https://github.com/martinf08/oplire/releases):

| Platform | Release target |
| --- | --- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| Windows x86-64 | `x86_64-pc-windows-msvc` |

Each release archive includes `oplire`, `proxy-rotate`, `proxy-hunt`,
`reset-opencode`, `reset-opencode-default`, this README, and the MIT license.
Linux archives also include the optional systemd unit. A `SHA256SUMS` file is
attached to each release. `reset-opencode-default` is Linux-only; it exits with
a clear unsupported-platform message elsewhere.

Download the archive for your OS/architecture, extract it, then install its
bundled executables on Linux:

```sh
sudo ./bin/oplire install
# Optional: enable the connectivity observer at boot (systemd only).
sudo ./bin/oplire install --with-service
```

Use `./bin/oplire install --help` to select a custom prefix, preview with
`--dry-run`, or install directly from a published release with
`./bin/oplire install --latest` (downloads and verifies all five binaries before
writing them). The default Linux/macOS prefix is `/usr/local`; Windows installs
to `%LOCALAPPDATA%\Programs\Oplire\bin`. On Windows, run the binaries from that
folder or add it to your user `PATH`.

## Binaries

- **`oplire`** — API bridge, health checks, model listing, and release installer.
- **`proxy-rotate`** — manual proxy-pool administration and an optional local
  forwarder/observer. Proxy changes are only explicit or in response to a
  connectivity failure; never a rate limit.
- **`proxy-hunt`** — intentionally disabled; it does not collect public proxy
  lists.
- **`reset-opencode`** — compatibility launcher for OpenCode. It does not
  perform a test generation or alter network settings.
- **`reset-opencode-default`** — separate Linux network-administration utility
  for WARP/WireGuard. Use `--dry-run` to inspect actions; `--purge` backs up
  matching files before removing them. It is never called by Oplire or by an
  HTTP 429 response.

The commands are separate Rust executables, built from the same Cargo package.
No Python or shell runtime is required to use or test the project. The systemd
unit is configuration, not an executable script.

## API bridge

The upstream must expose an OpenAI-compatible API, including
`POST /v1/chat/completions`. The bridge binds to loopback by default and
translates text-only Anthropic `POST /v1/messages` requests. It supports JSON
responses, server-sent event streams, model-list endpoints, and `GET /health`.
Images and tool use are rejected explicitly rather than silently discarded.

```sh
OPLIRE_UPSTREAM_URL=http://127.0.0.1:3000 \
OPLIRE_LISTEN=127.0.0.1:8080 \
oplire proxy
```

An upstream bearer token can be supplied via `OPLIRE_API_KEY`. Keep credentials
out of source files, command history, logs, and support reports. For remote
access, use an authenticated reverse proxy and appropriate network controls.

### Explicit egress proxy (including WARP local proxy)

By default, Oplire connects directly to its configured upstream and does not
inherit ambient `HTTP_PROXY`/`HTTPS_PROXY` settings. To route only Oplire's
upstream traffic through an explicitly chosen proxy, set `OPLIRE_EGRESS_PROXY`
or pass `--egress-proxy`:

```sh
# Set the port you configured in the WARP desktop app; WARP does not prescribe
# a universal port. HTTP and SOCKS5 local-proxy endpoints are supported.
OPLIRE_EGRESS_PROXY=http://127.0.0.1:PORT oplire proxy
# Or: OPLIRE_EGRESS_PROXY=socks5://127.0.0.1:PORT oplire proxy
```

This changes only the route used for Oplire's outbound upstream requests. It
does not modify system routing, the proxy pool, or WARP settings. Keep proxy
credentials out of command-line arguments and logs; prefer an environment
variable or a local endpoint.

The command-line equivalents are:

```sh
oplire proxy --listen 127.0.0.1:8080 --upstream http://127.0.0.1:3000
oplire check --url http://127.0.0.1:3000/health --attempts 3
oplire models
```

`oplire check` uses bounded attempts only for network errors and transient
server errors. HTTP 429 is returned immediately, including `Retry-After` when
provided by the endpoint.

## Proxy pool

The pool is for proxies you are authorized to use and legitimate networking
purposes only. Proxies are added manually:

```sh
proxy-rotate init
proxy-rotate add 'http://user:pass@proxy.example:8080'
proxy-rotate list
proxy-rotate use ID
proxy-rotate status
```

`proxy-rotate 429` is informational and does not create or modify state.
`proxy-rotate rotate --reason 'HTTP 429 quota'` is rejected. The optional
observer reports rate limits without switching proxies or replaying requests;
connectivity checks use a single probe and treat an HTTP response (including
429) as proof that the route is reachable. Proxy credentials are redacted in
listing output and JSON exports.

Manual commands such as `rotate`, `use`, `test`, and `refresh` may alter the
configured pool or send network probes when explicitly run. Public proxies are
untrusted; do not send them credentials or sensitive data.

## WARP or an external proxy?

For routing **one desktop application** through Cloudflare, the preferred
starting point is WARP's **Local proxy** mode, if it is available and allowed by
your policy. Configure it in the WARP desktop app under **Preferences →
Advanced → Configure Proxy**, choose a port and enable Local proxy, then point
`OPLIRE_EGRESS_PROXY` at its local HTTP or SOCKS5 endpoint as above. Only
applications configured to use that endpoint use WARP; other device traffic
continues over the normal connection.

Use WARP's **Traffic and DNS** mode only when you intend to route the device's
traffic broadly. It is not an anonymity service and does not let you choose a
country. An external proxy is an alternative only when you trust and are
authorized to use its operator; it can observe connection metadata, and a
public proxy should never receive sensitive traffic. For this Oplire use case,
WARP Local proxy is generally the simpler choice for a single desktop app; a
trusted external proxy is appropriate when you specifically need that proxy's
network or egress policy. Neither option may be used to evade a provider quota.

See Cloudflare's current documentation for [WARP modes](https://developers.cloudflare.com/warp-client/warp-modes/) and the [WARP client](https://developers.cloudflare.com/warp-client/).

## Build and test from source

Requirements: Rust stable and Cargo. SQLite is bundled for reproducible builds.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --bins --locked
```

The tests use temporary databases and local fixtures; they do not use real
proxies or change host networking. CI checks formatting, lint, tests, and release
builds. A version tag builds target-specific archives and publishes them with
SHA-256 checksums.

## License and attribution

Oplire is distributed under the MIT License. The Anthropic API transformation
and portions of the bridge structure are adapted from `BerkeOruc/oplire`; see
`LICENSE` for the required copyright and license notices.

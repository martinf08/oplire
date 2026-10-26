# Security

Oplire is a local, text-only API bridge and an optional proxy administration
toolkit. It is not an authentication gateway or a network anonymity service.

- Keep the bridge on loopback. Remote access requires an authenticated reverse
  proxy and appropriate firewall rules.
- Use only trusted, authorized upstreams and proxies. Keep API keys and proxy
  passwords out of issues, logs, command-line arguments, and committed files.
- SHA-256 manifests detect download corruption; they are not independently
  signed attestations of release provenance.
- HTTP 429/quota must never trigger request replay, a proxy switch, or a WARP
  reset. Honor the provider's limits and `Retry-After`.
- `reset-opencode-default` is a privileged Linux administration command. Review
  its dry run before executing it; it can change routes and remove backed-up
  WARP files when explicitly requested.

## Reporting a vulnerability

Do not publish credentials or exploit details in a public issue. Use GitHub's
**Security → Report a vulnerability** on this repository when available. If
private reporting is unavailable, open an issue requesting a private contact
without including sensitive details.

Use the latest release. Older releases are not maintained as separate branches.

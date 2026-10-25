# WARP and Oplire

This project does not control Cloudflare WARP or use it to respond to HTTP 429
errors or bypass quotas. The bridge and its scripts do not reset or change
network identity when a provider reports a limit.

If WARP is managed separately on your machine, configure it according to
Cloudflare's official documentation and your organization's network policies.
`reset-opencode-default` is a separate legacy administration script retained
for compatibility; it can modify routing, and its `--purge` option deletes
files. It is never invoked by `oplire proxy`, `oplire check`, or
`reset-opencode`.

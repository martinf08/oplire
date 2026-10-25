---
name: oplire
description: Check an OpenCode endpoint or run the local Anthropic bridge without bypassing provider limits.
---

# Oplire: bridge and diagnostics

## HTTP 429 and quotas

An HTTP 429 indicates a provider limit. Do not change the IP address, proxy,
or WARP to evade it, and do not automatically replay the request. Honor
`Retry-After` and the provider's terms. The Oplire bridge forwards the status
and `Retry-After` header without replaying the request.

## Bridge

```sh
OPLIRE_UPSTREAM_URL=http://127.0.0.1:3000 oplire proxy
```

The bridge listens on `127.0.0.1:8080` by default. The Anthropic-compatible
routes include `POST /v1/messages` and `GET /v1/models`.

## Explicit diagnostics

```sh
oplire check --url http://127.0.0.1:3000/health
oplire models
```

Checks may retry bounded network or transient server errors, but stop
immediately on HTTP 429.

## Proxy management

The legacy `proxy-rotate` scripts support manual administration of authorized
proxies. Do not use them to evade provider limits. `proxy-hunt` is disabled,
and the watcher does not switch proxies on HTTP 429. Automatic changes are
reserved for connectivity failures.

## Legacy launcher

`reset-opencode` is an OpenCode launcher alias. It does not reset networking or
send a test generation. An explicit check is available with
`reset-opencode --check URL`; any remaining arguments are passed to OpenCode.

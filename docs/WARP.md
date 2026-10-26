# WARP and Oplire

Oplire never responds to HTTP 429/quota by retrying a request, switching a
proxy, resetting WARP, or changing network identity. The bridge forwards the
upstream limit response and `Retry-After` header as received.

## Recommended choice for Oplire

If the goal is to route just Oplire's upstream requests from a desktop through
Cloudflare, use WARP's **Local proxy** mode when available and permitted:

1. In the WARP desktop client, open **Preferences → Advanced → Configure
   Proxy**, enable the local proxy, and select a listening port.
2. Start Oplire with the endpoint and protocol shown/configured in WARP:

   ```sh
   OPLIRE_EGRESS_PROXY=http://127.0.0.1:PORT oplire proxy
   # Or, if the local endpoint is SOCKS5:
   OPLIRE_EGRESS_PROXY=socks5://127.0.0.1:PORT oplire proxy
   ```

   The port is the one configured on that device; it is not a universal
   hard-coded port. Oplire supports explicit HTTP(S) and SOCKS5 egress. This
   setting affects only requests from Oplire to its upstream. By default,
   Oplire makes a direct connection and does not inherit generic
   `HTTP_PROXY`/`HTTPS_PROXY` variables.

Cloudflare's **Local proxy** mode is desktop-only and routes only applications
configured to use it; other traffic remains on the normal connection. **Traffic
and DNS** mode routes device traffic more broadly, so use it only if that is
intended. WARP does not provide anonymity or let users select a country.

An external proxy is suitable when its operator is trusted and authorized, and
when you specifically need that proxy's network or egress policy. Avoid public
proxies for credentials or sensitive traffic. Neither WARP nor any proxy should
be used to evade a provider's quota.

`proxy-rotate` is a separate manually managed pool for authorized proxies. It
does not configure WARP, and its optional connectivity observer never changes
routes on HTTP 429. `reset-opencode-default` is a separate Linux administration
tool that can modify routing only when explicitly run; review `--dry-run` and
never use it as a quota response.

Official references: [Cloudflare WARP modes](https://developers.cloudflare.com/warp-client/warp-modes/) and [Cloudflare WARP client](https://developers.cloudflare.com/warp-client/).

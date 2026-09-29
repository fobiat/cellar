# HTTPS and remote access

Cellar's operator console runs at full engine privilege. The supported HTTPS
deployment is a TLS-terminating reverse proxy in front of a private Cellar HTTP
listener. Cellar does not load certificates or terminate TLS itself. Use modern
TLS 1.2/1.3; obsolete SSL protocols are not supported.

## Local setup

For a browser on the same machine:

```toml
[web]
enabled = true
bind = "127.0.0.1:8081"
auth = "password"
```

Visit `http://127.0.0.1:8081` to complete first-run password setup. Cellar saves
an owner-only Argon2 hash beside the active config. Alternatively, use
`cellar hash-password` and supply `CELLAR_WEB_PASSWORD_HASH` through a protected
service environment or secret store. Never put a plaintext password in TOML.

## Public or LAN HTTPS

Provision the password hash on the host before enabling remote access. Add:

```toml
[web]
enabled = true
bind = "127.0.0.1:8081"
auth = "password"
secure_cookies = true
public_origin = "https://cellar.example.com"

[web.tailscale]
enabled = false
```

`public_origin` is exactly a canonical HTTPS origin, including a non-default
port when needed, with no trailing slash, path, credentials, query or fragment.
It requires a provisioned `CELLAR_WEB_PASSWORD_HASH`, password authentication,
Secure cookies and the direct Tailscale listener disabled. Restart Cellar after
changing this policy. Live profile switching cannot change authentication or
HTTPS policy.

Install Caddy and adapt [`deploy/Caddyfile`](../deploy/Caddyfile):

```caddyfile
cellar.example.com {
    tls {
        protocols tls1.2 tls1.3
    }
    reverse_proxy 127.0.0.1:8081
}
```

Point DNS at the host, allow ports 80/443 to Caddy, and keep 8081 private.
Caddy obtains and renews a publicly trusted certificate and redirects HTTP to
HTTPS. For a private LAN name, add `tls internal` instead and install Caddy's
root CA on each client. Do not train operators to ignore certificate errors.
Check certificate renewal and expiry as part of host monitoring.

The proxy must preserve the external `Host` and support WebSocket upgrades.
For nginx, set `proxy_set_header Host $http_host`, forward Upgrade and Connection
for `/api/events`, and proxy to the private listener. Restrict backend access
with loopback or a firewall; a plaintext backend reachable from the internet is
not secured by HTTPS at the proxy.

Cellar deliberately ignores `Forwarded`, `X-Forwarded-Host` and
`X-Forwarded-Proto` for trust decisions. The configured origin is authoritative.
Unexpected hosts receive 421. Browser mutations and WebSocket connections must
match the host and exact HTTPS origin. HTTP origins are refused in Secure-cookie
mode. There is no user-controlled redirect target.

HTTPS mode emits a one-year HSTS policy for this host, without `includeSubDomains`
or preload. Operator cookies are `__Host-cellar_session`, Secure, HttpOnly,
SameSite=Strict, and Path=/. UI, API and service-worker responses use `no-store`.
The worker does not cache pages or data. Frame embedding is denied, referrers are
suppressed, and camera/microphone/geolocation permissions are disabled.

## Tailscale Serve

Use the same private-listener configuration, with `public_origin` set to the
HTTPS origin that Tailscale Serve reports:

```sh
tailscale serve --https=443 http://127.0.0.1:8081
tailscale serve status
```

Disable Cellar's direct listener with `[web.tailscale] enabled = false`. This
avoids a second HTTP entrance alongside HTTPS. Tailscale's transport encryption
and browser HTTPS solve different boundaries; an HTTP tailnet URL is not HTTPS.

The optional direct Tailscale listener remains available for private tailnet
deployments without `public_origin`. It requires a provisioned password and
uses the port in `web.bind`. It is not the recommended remote browser path.

## Verification

Open the HTTPS URL and check the certificate issuer, name and expiry. Sign in,
confirm the live indicator connects over WSS, send `status`, then sign out and
confirm the session is revoked. Verify:

```sh
curl -I https://cellar.example.com/
curl -I http://cellar.example.com/
```

The first should show `Cache-Control: no-store`, HSTS and frame denial. The
second should redirect at the proxy. From another machine, port 8081 must be
unreachable. With `public_origin` configured, a direct loopback request using
its default Host receives 421; supply the configured Host only for local probes.

The browser suite includes a disposable TLS reverse proxy, real login and
host-only cookie checks, WSS, wrong-scheme refusal, forwarded-header spoofing and
logout. It uses a temporary self-signed test certificate; production clients
must trust the actual deployment certificate.

## Phones and terminal clients

Open the HTTPS URL in a current browser and optionally use Add to Home Screen.
The [operator workspace guide](WEB-UI.md) covers phone navigation, full status,
keyboard use and recovery.

CLI and TUI clients must use the same HTTPS URL when `public_origin` is set.
Treat `CELLAR_SESSION` as a bearer credential and supply it through a protected
environment, never a shared script or shell-history assignment. The read-only
`CELLAR_API_TOKEN` does not authorize console commands.

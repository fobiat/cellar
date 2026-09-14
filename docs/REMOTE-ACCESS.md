# Remote web access

Cellar's web panel is responsive and works in current desktop and mobile
browsers. The browser, tray, and remote TUI use the same operator session. Keep
the Cellar listener private and put TLS or a private network boundary in front
of it before allowing remote access.

## Local machine

The safe default is loopback:

```toml
[web]
enabled = true
bind = "127.0.0.1:8081"
auth = "password"
```

Open `http://127.0.0.1:8081` on the host. On first visit, Cellar asks you to
choose the operator password and saves only its owner-only Argon2 hash. For
automation, generate the hash with `cellar hash-password` and provide it as
`CELLAR_WEB_PASSWORD_HASH`.

## LAN access

For a small trusted LAN, bind the web listener to the host's LAN interface or
to `0.0.0.0:8081`, keep password authentication enabled, and allow TCP 8081
only from the LAN in the host firewall. Direct HTTP is suitable only for a
temporary private network; set `secure_cookies = false` for that case.

The preferred LAN setup is a reverse proxy with a certificate:

```caddyfile
cellar.lan {
    tls internal
    reverse_proxy 127.0.0.1:8081
}
```

Keep Cellar on `127.0.0.1:8081`, set `secure_cookies = true`, and install the
proxy's internal CA certificate on each phone or tablet. Use the proxy URL in
the browser instead of exposing Cellar directly.

## Tailscale

Cellar can add a second listener on the host's Tailscale IPv4 address when
`tailscale.enabled = true`, `web.auth` is not `none`, and a web password is
configured. It uses the same port as `web.bind`, requires the operator
password, and appears in the
Addresses panel. Disable it with `tailscale.enabled = false` when the host
should not serve the UI directly over the tailnet.

For the preferred HTTPS path, keep Cellar on loopback and publish it through
Tailscale Serve:

```sh
tailscale serve --https=443 http://127.0.0.1:8081
tailscale serve status
```

Open the HTTPS URL shown by `tailscale serve status` from another device on the
same tailnet. Tailscale provides the private network boundary, while Cellar's
password still protects the operator console. `secure_cookies = true` is the
right setting for this HTTPS path.

For a direct tailnet connection, open `http://<tailscale-ip>:<web-port>` on the
phone and sign in with the Cellar operator password. This direct listener is
still password-protected. Use Tailscale Serve when HTTPS is required.

For a Kubernetes deployment, expose the web Service through a Tailscale
Ingress or an HTTPS ingress controller. Do not publish the bridge port to the
tailnet unless the gamemode needs it there.

## Mobile devices

Open the LAN or Tailscale HTTPS URL in Safari, Chrome, or another current
mobile browser, sign in, then use Add to Home Screen if a launcher is useful.
The layout switches to stacked action rows and touch-sized controls. No mobile
app or separate mobile API is required.

For a terminal dashboard from a laptop or desktop, use:

```sh
CELLAR_SESSION='your-session-cookie' cellar tui \
  --url https://cellar.example.ts.net
```

The session is a bearer credential. Do not put it in shell history, a systemd
unit, a Docker image, or a shared chat. The read-only `CELLAR_API_TOKEN`
surface is for integrations and does not authorize console commands.

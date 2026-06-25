# Runbook – Private network (Headscale overlay)

Stand up a WireGuard overlay so two machines on different networks reach each
other for Hearth voice (raw UDP) and screenshare (webrtc) with **no NAT-traversal
code**. Headscale is the self-hosted control server (Tailscale's coordination
protocol); each machine runs the stock Tailscale client. The backend runs on the
dev PC over the overlay (not the VPS).

> Scope: a minimal, working overlay. TLS on the Hearth backend, self-hosted DERP,
> and a dedicated always-on backend node are out of scope (WireGuard already
> encrypts; the public DERP map handles relay fallback).

## 0. Prerequisites

- A public VPS with a DNS name pointing at it, e.g. `headscale.example.net`.
- Docker + Docker Compose on the VPS.
- Two client machines (dev PC + friend's PC) with admin rights to install Tailscale.
- The Hearth dockerized stack on the dev PC (`make up` works locally).

## 1. VPS – run Headscale in Docker

Headscale needs a reachable `server_url` over HTTPS. Use Headscale's built-in
ACME (Let's Encrypt) so no separate reverse proxy is required.

```bash
# On the VPS
sudo mkdir -p /etc/headscale /var/lib/headscale
curl -fsSL https://raw.githubusercontent.com/juanfont/headscale/main/config-example.yaml \
  | sudo tee /etc/headscale/config.yaml >/dev/null
```

Edit `/etc/headscale/config.yaml` and set (leave everything else at defaults):

```yaml
server_url: https://headscale.example.net      # your DNS name, HTTPS
listen_addr: 0.0.0.0:443                        # ACME terminates TLS here
tls_letsencrypt_hostname: headscale.example.net
tls_letsencrypt_challenge_type: TLS-ALPN-01     # needs port 443 reachable
# DERP: keep the embedded public map for relay fallback (default `urls` block)
```

Open the firewall: TCP 443 (control + ACME) and UDP 3478 (STUN, used by DERP).

Run it with Compose (`/etc/headscale/compose.yml`):

```yaml
services:
  headscale:
    image: headscale/headscale:latest
    container_name: headscale
    restart: unless-stopped
    command: serve
    network_mode: host                 # simplest path for ACME on :443
    volumes:
      - /etc/headscale:/etc/headscale
      - /var/lib/headscale:/var/lib/headscale
```

```bash
cd /etc/headscale && sudo docker compose up -d
sudo docker compose logs -f headscale   # confirm "listening" + a cert was issued
```

## 2. VPS – create a user and a preauth key

```bash
# `headscale` runs inside the container; alias for brevity
hs() { sudo docker exec headscale headscale "$@"; }

hs users create hearth
hs preauthkeys create --user hearth --reusable --expiration 24h
# → copy the printed key, referred to below as <PREAUTH>
```

`--reusable` lets both machines use the same key; drop it for one-shot keys.

## 3. Each node – install Tailscale and join

On **both** the dev PC and the friend's PC:

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up \
  --login-server=https://headscale.example.net \
  --authkey=<PREAUTH>

tailscale status        # both nodes listed, each with a 100.64.x.x address
tailscale ip -4         # this machine's overlay IP (100.64.x.x)
```

Record each machine's `100.64.x.x` overlay IP – you need them next.

## 4. Backend – run the dockerized stack on the dev PC

```bash
# On the dev PC, in the hearth repo
make up
tailscale ip -4          # e.g. 100.64.0.1  → the backend's overlay IP
```

The backend listens on `0.0.0.0:8080`, so it is reachable at the dev PC's
overlay IP. Confirm from the friend's PC:

```bash
curl http://100.64.0.1:8080/health   # adjust to an existing health/route
```

## 5. Clients – set the three env vars and launch

On **each** machine, point the desktop at the backend's overlay address and
advertise **this machine's own** overlay IP:

```bash
# Dev PC (backend overlay IP = its own here, 100.64.0.1)
export HEARTH_HTTP=http://100.64.0.1:8080
export HEARTH_WS=ws://100.64.0.1:8080
export HEARTH_ADVERTISE_IP=100.64.0.1

# Friend's PC (backend overlay IP is the dev PC; advertise the friend's own IP)
export HEARTH_HTTP=http://100.64.0.1:8080
export HEARTH_WS=ws://100.64.0.1:8080
export HEARTH_ADVERTISE_IP=100.64.0.2
```

`HEARTH_ADVERTISE_IP` is the one that matters for cross-machine voice: voice
sockets bind `0.0.0.0` (they receive on the overlay already), but the *advertised*
endpoint must be the overlay IP, not the default-route LAN IP. Then launch the
desktop on each machine.

## 6. Verify

**LAN pre-check (no overlay, proves the advertise fix first):** put both machines
on the same LAN; set `HEARTH_ADVERTISE_IP=<each machine's LAN IP>` and
`HEARTH_HTTP`/`HEARTH_WS` to the backend host. Join voice both directions and start
a screenshare. If this works, the advertise fix is correct independent of Headscale.

**Overlay test:** both machines on the headnet with `HEARTH_ADVERTISE_IP=<overlay
IP>`, pointed at the backend's overlay address. Then:

- Join voice **both** directions; confirm two-way audio.
- Start a screenshare; confirm the remote sees it.
- Check the path is direct, not relayed:

```bash
tailscale ping 100.64.0.2     # from the dev PC
# Expect: "pong ... via DIRECT" (not "via DERP") on a good network path
```

DERP relaying still yields working media; a `DIRECT` path just confirms
WireGuard hole-punching succeeded.

## Troubleshooting

- **No audio one direction:** that side's `HEARTH_ADVERTISE_IP` is wrong (likely
  the LAN IP, not `100.64.x.x`). Re-check `tailscale ip -4` and re-export.
- **Client can't reach backend:** confirm `make up` is running and
  `curl http://<backend-overlay-ip>:8080/...` works from the other node.
- **Node won't join:** preauth key expired or non-reusable – mint a new one
  (step 2). Check `hs nodes list` on the VPS.
- **`tailscale ping` always DERP:** restrictive NAT/firewall; media still flows
  via relay. Out of scope to fix here.

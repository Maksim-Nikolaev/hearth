# Phase A — Private network (Headscale overlay)

Date: 2026-06-25
Status: Approved (design)

## Goal

Let two machines on different networks reach each other over a private WireGuard
overlay so the existing raw-UDP P2P voice and webrtc screenshare work
**cross-machine with no NAT-traversal code**. This validates the whole
architecture over a real network and defers public NAT traversal indefinitely.

## Scope

- **In:** a Headscale overlay (control server on a public VPS, Tailscale clients
  on the two machines), the dockerized backend reachable over the overlay, a small
  voice **endpoint-advertise** fix (`HEARTH_ADVERTISE_IP`), and a runbook.
- **Out (deferred):** public NAT traversal / coturn, TLS on the backend (the
  overlay already encrypts via WireGuard), self-hosting DERP relays, production
  hardening, a dedicated always-on backend node.

## Why it's mostly infra

The current code already supports the overlay with one gap:
- **Backend URL** is env-driven (`HEARTH_HTTP`/`HEARTH_WS`, `desktop/src/config.rs`)
  → clients just point at the backend's overlay address. No change.
- **Screenshare** (`webrtcbin` + STUN/ICE, `engine/src/flow_peer.rs`) gathers host
  candidates on **all** interfaces incl. the overlay → traverses it automatically.
  No change.
- **Voice (raw UDP)** advertises its endpoint via `local_ip()`
  (`engine/src/voice_udp.rs:106`, `engine/src/audio/native_voice.rs:627`), which
  uses the `8.8.8.8` route-trick → returns the **default-route IP, not the overlay
  IP**. The sockets bind `0.0.0.0` (so they *receive* on the overlay); only the
  *advertised* IP is wrong. **This is the one code fix.**

## Topology

- **Headscale control server** (Docker) on a public VPS with a domain + TLS
  (behind the existing reverse proxy or Headscale's built-in ACME). Coordinates
  node registration; uses the public DERP map for relay fallback.
- **Overlay clients:** dev box + friend's box run **Tailscale**, `tailscale up
  --login-server=https://<headscale-domain> --authkey=<preauth>`. Each gets a
  `100.64.x.x` IP + MagicDNS name.
- **Backend:** the dockerized stack runs on the dev box (`make up`), reachable
  over the overlay at the dev box's overlay IP / MagicDNS, port 8080.
- **Media:** voice (raw UDP) and screenshare (webrtc) flow host-to-host over the
  overlay (WireGuard), DERP-relayed only if direct fails.

## Components

### 1. Engine — `HEARTH_ADVERTISE_IP` (the only code change)

- Factor the duplicated `local_ip()` into one helper used by both `voice_udp.rs`
  and `native_voice.rs` (DRY). New `engine/src/net.rs`:
  `pub fn advertised_ip() -> String` — returns `HEARTH_ADVERTISE_IP` when set and
  non-empty, else the existing route-trick, else `127.0.0.1`.
- The env-precedence is a pure helper `pick_advertise_ip(env: Option<String>,
  detected: &str) -> String` (returns the trimmed env value when non-empty, else
  `detected`) — unit-tested.
- Both voice paths call `advertised_ip()` for the endpoint they hand the peer.

### 2. Config (no code) — client env

Each client sets:
- `HEARTH_HTTP=http://<backend-overlay-ip>:8080`, `HEARTH_WS=ws://<…>:8080`
- `HEARTH_ADVERTISE_IP=<this machine's overlay IP>`

### 3. Runbook — `docs/runbooks/private-network-headscale.md`

A concrete, copy-paste runbook:
1. **VPS:** run Headscale in Docker (config `server_url`, listen addr, public DERP
   map), front it with TLS (reverse proxy or built-in ACME). Create a user +
   reusable preauth key (`headscale users create`, `headscale preauthkeys create`).
2. **Each node:** install Tailscale, `tailscale up --login-server=… --authkey=…`;
   confirm `100.64.x.x` + `tailscale status`.
3. **Backend:** `make up` on the dev box; note its overlay IP (and MagicDNS name).
4. **Clients:** set the three env vars above; launch the desktop.
5. **Verify** (below).

## Verification (user runs the two-machine test)

- **LAN pre-check (zero infra):** two machines on the same LAN, each with
  `HEARTH_ADVERTISE_IP=<its LAN IP>` and `HEARTH_HTTP/WS` → the backend host →
  join voice both ways + screenshare. Proves the advertise fix before the overlay.
- **Overlay test:** both machines on the headnet, `HEARTH_ADVERTISE_IP=<overlay
  IP>`, clients → backend overlay address → voice both ways + screenshare across
  the two networks. `tailscale ping <peer>` should show a **direct** connection
  (not DERP-relayed) on a good path.
- Engine unit test: `pick_advertise_ip` precedence (env override vs detected).

## Non-goals

- Public NAT traversal (STUN hole-punch + coturn) — the overlay removes the need.
- TLS on the Hearth backend itself (WireGuard encrypts the overlay).
- Self-hosted DERP, MagicDNS-as-requirement, a dedicated backend node, automation
  of the VPS provisioning.
</content>

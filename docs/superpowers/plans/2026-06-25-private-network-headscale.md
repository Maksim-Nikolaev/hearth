# Private Network (Headscale overlay) — Phase A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the raw-UDP voice transport advertise a correct, overridable endpoint IP so two machines reach each other over a WireGuard (Headscale/Tailscale) overlay with no NAT-traversal code, plus a copy-paste runbook to stand the overlay up.

**Architecture:** One new `engine/src/net.rs` module owns endpoint-IP selection. A pure, unit-tested helper `pick_advertise_ip(env, detected)` decides env-override-vs-detected; `advertised_ip()` wires it to the real env var and the existing `8.8.8.8` route-trick. Both voice paths (`voice_udp.rs`, `native_voice.rs`) drop their duplicated `local_ip()` and call `net::advertised_ip()`. Everything else (backend URL via `HEARTH_HTTP`/`HEARTH_WS`, webrtc ICE) already traverses the overlay unchanged.

**Tech Stack:** Rust (engine crate, std only — no new dependencies), GStreamer (untouched here), Headscale + Tailscale (runbook, ops only).

## Global Constraints

- No new crate dependencies — `net.rs` uses `std::net` and `std::env` only.
- Helper `pick_advertise_ip(env: Option<String>, detected: &str) -> String`: trimmed env value when non-empty, else `detected`.
- `advertised_ip() -> String`: returns `HEARTH_ADVERTISE_IP` when set and non-empty, else the route-trick, else `127.0.0.1`.
- Env var name is exactly `HEARTH_ADVERTISE_IP`.
- Route-trick behaviour preserved verbatim: bind `0.0.0.0:0`, connect `8.8.8.8:80`, read `local_addr().ip()`, fall back to `127.0.0.1`. No packet is actually sent.
- Comments follow the repo style: present-tense invariants, no history/fix references, En dashes not Em dashes.
- Do not commit or push unless the user explicitly asks.

---

### Task 1: `net.rs` endpoint-IP helper (TDD)

**Files:**
- Create: `engine/src/net.rs`
- Modify: `engine/src/lib.rs:1-11` (add `pub mod net;`)
- Test: inline `#[cfg(test)]` module in `engine/src/net.rs`

**Interfaces:**
- Consumes: nothing (leaf module, std only).
- Produces:
  - `pub fn pick_advertise_ip(env: Option<String>, detected: &str) -> String`
  - `pub fn advertised_ip() -> String`

- [ ] **Step 1: Add the module declaration**

In `engine/src/lib.rs`, keep the list alphabetical — insert `pub mod net;` between `pub mod hotkey;` and `pub mod screen;`:

```rust
pub mod audio;
pub mod capture;
pub mod encoders;
pub mod flow;
pub mod flow_peer;
pub mod hotkey;
pub mod net;
pub mod screen;
pub mod session;
pub mod signaling;
pub mod voice_udp;
```

- [ ] **Step 2: Write the failing test**

Create `engine/src/net.rs` with only the test module and a stub so it compiles but fails:

```rust
//! Endpoint-IP selection for the raw-UDP voice transport. The advertised IP is
//! the address a peer dials back on, which on an overlay (WireGuard/Tailscale)
//! differs from the default-route IP — hence the `HEARTH_ADVERTISE_IP` override.

/// Pick the IP to advertise: the trimmed `HEARTH_ADVERTISE_IP` value when set and
/// non-empty, otherwise the auto-detected route IP.
pub fn pick_advertise_ip(env: Option<String>, detected: &str) -> String {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::pick_advertise_ip;

    #[test]
    fn env_override_wins_when_set() {
        assert_eq!(pick_advertise_ip(Some("100.64.0.2".into()), "192.168.1.5"), "100.64.0.2");
    }

    #[test]
    fn falls_back_to_detected_when_env_absent() {
        assert_eq!(pick_advertise_ip(None, "192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn empty_or_whitespace_env_falls_back_to_detected() {
        assert_eq!(pick_advertise_ip(Some("".into()), "192.168.1.5"), "192.168.1.5");
        assert_eq!(pick_advertise_ip(Some("   ".into()), "192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn env_value_is_trimmed() {
        assert_eq!(pick_advertise_ip(Some("  100.64.0.2  ".into()), "192.168.1.5"), "100.64.0.2");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p hearth-engine --lib net::tests`
Expected: tests compile and FAIL (panic `not implemented` from `unimplemented!()`).

(If the engine crate name differs, use the name from `engine/Cargo.toml`'s `[package] name`. Confirm with `cargo test -p hearth-engine --lib net::tests 2>&1 | head` or fall back to `cargo test --lib net::tests` run from `engine/`.)

- [ ] **Step 4: Implement the pure helper**

Replace the `unimplemented!()` body:

```rust
pub fn pick_advertise_ip(env: Option<String>, detected: &str) -> String {
    match env {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => detected.to_string(),
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p hearth-engine --lib net::tests`
Expected: PASS (4 tests).

- [ ] **Step 6: Add the env-wired `advertised_ip()`**

Append below `pick_advertise_ip` (above the `#[cfg(test)]` module):

```rust
/// Best-effort local IPv4 a peer can dial us on. Honors `HEARTH_ADVERTISE_IP`
/// (the overlay address on a WireGuard/Tailscale net), else discovers the
/// default-route interface via a connect to a public address (no packet is
/// sent), else loopback for same-machine testing.
pub fn advertised_ip() -> String {
    pick_advertise_ip(std::env::var("HEARTH_ADVERTISE_IP").ok(), &detect_route_ip())
}

/// Route-trick: the source IP the kernel would use to reach a public address,
/// i.e. the default-route interface. Loopback fallback when offline.
fn detect_route_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}
```

- [ ] **Step 7: Verify the crate builds**

Run: `cargo build -p hearth-engine`
Expected: builds clean (a temporary `dead_code` warning for unused `advertised_ip`/`detect_route_ip` is acceptable here; Task 2 wires the call sites).

- [ ] **Step 8: Commit**

```bash
git add engine/src/net.rs engine/src/lib.rs
git commit -m "feat(engine): net::advertised_ip with HEARTH_ADVERTISE_IP override (TDD)"
```

---

### Task 2: Wire both voice paths to `net::advertised_ip()` and delete the duplicates

**Files:**
- Modify: `engine/src/voice_udp.rs:103-114` (delete `local_ip`), `engine/src/voice_udp.rs:293` (call site)
- Modify: `engine/src/audio/native_voice.rs:626-635` (delete `local_ip`), `engine/src/audio/native_voice.rs:582` (call site)

**Interfaces:**
- Consumes: `crate::net::advertised_ip()` from Task 1.
- Produces: nothing new (removes two private `local_ip` fns).

- [ ] **Step 1: Replace the call site in `voice_udp.rs`**

At `engine/src/voice_udp.rs:293`, change:

```rust
        format!("{}:{}", local_ip(), self.local_port)
```

to:

```rust
        format!("{}:{}", crate::net::advertised_ip(), self.local_port)
```

- [ ] **Step 2: Delete the duplicated `local_ip` in `voice_udp.rs`**

Remove the whole block at `engine/src/voice_udp.rs:103-114` (the doc comment + `fn local_ip() -> String { ... }`):

```rust
/// Best-effort local IPv4 the peer can reach us on. Uses the route to a public
/// address to discover the LAN interface (no packet is sent). Falls back to
/// loopback, which still works for same-machine testing.
fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}
```

- [ ] **Step 3: Replace the call site in `native_voice.rs`**

At `engine/src/audio/native_voice.rs:582`, change:

```rust
        Ok(format!("{}:{}", local_ip(), local_port))
```

to:

```rust
        Ok(format!("{}:{}", crate::net::advertised_ip(), local_port))
```

- [ ] **Step 4: Delete the duplicated `local_ip` in `native_voice.rs`**

Remove the whole block at `engine/src/audio/native_voice.rs:626-635`:

```rust
/// Best-effort LAN IP (UDP-connect trick, no packet sent); loopback fallback.
fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}
```

- [ ] **Step 5: Verify the crate builds with no `local_ip` left**

Run: `cargo build -p hearth-engine && rg -n "fn local_ip" engine/src`
Expected: builds clean; `rg` prints nothing (exit 1), confirming both duplicates are gone and no `dead_code` warning remains for `advertised_ip`.

- [ ] **Step 6: Run the full engine test suite**

Run: `cargo test -p hearth-engine`
Expected: PASS (includes `net::tests` from Task 1).

- [ ] **Step 7: Commit**

```bash
git add engine/src/voice_udp.rs engine/src/audio/native_voice.rs
git commit -m "refactor(engine): voice paths advertise via net::advertised_ip (DRY)"
```

---

### Task 3: Runbook — stand up the Headscale overlay

**Files:**
- Create: `docs/runbooks/private-network-headscale.md`

**Interfaces:**
- Consumes: the three env vars (`HEARTH_HTTP`, `HEARTH_WS`, `HEARTH_ADVERTISE_IP`) and `make up` from the dockerized stack.
- Produces: ops documentation only (no code).

- [ ] **Step 1: Write the runbook**

Create `docs/runbooks/private-network-headscale.md` with the full content below. It must be copy-paste runnable; placeholders are written as `<…>` and called out.

````markdown
# Runbook — Private network (Headscale overlay)

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

## 1. VPS — run Headscale in Docker

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

## 2. VPS — create a user and a preauth key

```bash
# `headscale` runs inside the container; alias for brevity
hs() { sudo docker exec headscale headscale "$@"; }

hs users create hearth
hs preauthkeys create --user hearth --reusable --expiration 24h
# → copy the printed key, referred to below as <PREAUTH>
```

`--reusable` lets both machines use the same key; drop it for one-shot keys.

## 3. Each node — install Tailscale and join

On **both** the dev PC and the friend's PC:

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up \
  --login-server=https://headscale.example.net \
  --authkey=<PREAUTH>

tailscale status        # both nodes listed, each with a 100.64.x.x address
tailscale ip -4         # this machine's overlay IP (100.64.x.x)
```

Record each machine's `100.64.x.x` overlay IP — you need them next.

## 4. Backend — run the dockerized stack on the dev PC

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

## 5. Clients — set the three env vars and launch

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
- **Node won't join:** preauth key expired or non-reusable — mint a new one
  (step 2). Check `hs nodes list` on the VPS.
- **`tailscale ping` always DERP:** restrictive NAT/firewall; media still flows
  via relay. Out of scope to fix here.
````

- [ ] **Step 2: Verify the file renders and links are intact**

Run: `rg -n "HEARTH_ADVERTISE_IP|tailscale up|make up" docs/runbooks/private-network-headscale.md`
Expected: matches for all three (env var, join command, backend command) — confirms the key sections are present.

- [ ] **Step 3: Commit**

```bash
git add docs/runbooks/private-network-headscale.md
git commit -m "docs(runbook): private network Headscale overlay setup + verify"
```

---

## Self-Review

**Spec coverage:**
- Component 1 (engine `HEARTH_ADVERTISE_IP`, DRY helper, pure `pick_advertise_ip`, unit test) → Tasks 1–2.
- Component 2 (client env, no code) → documented in runbook step 5 (Task 3).
- Component 3 (runbook with the 5 steps + verify) → Task 3.
- Verification section (LAN pre-check, overlay test, `tailscale ping` direct, unit test) → runbook step 6 + Task 1 tests.
- Non-goals (NAT traversal, backend TLS, self-hosted DERP, dedicated node) → noted as out-of-scope in the runbook header. No tasks, correct.

**Placeholder scan:** Runbook `<…>` tokens are intentional ops placeholders (DNS name, preauth key, overlay IPs), each explained in context. No code placeholders.

**Type consistency:** `pick_advertise_ip(Option<String>, &str) -> String` and `advertised_ip() -> String` are used identically in Tasks 1 and 2; call sites use `crate::net::advertised_ip()`.

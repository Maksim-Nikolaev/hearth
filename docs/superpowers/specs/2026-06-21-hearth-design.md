# Hearth – Design Document

_A self-hosted, low-latency voice + high-fidelity screenshare app for a small group of close friends._

Status: approved design (2026-06-21). Codename retired: `BUDDY-NET` / "Better Discord".

---

## 1. Vision & Scope

Hearth is a standalone desktop application where 2–3 trusted friends get three
**independent** real-time media flows – low-latency voice, high-fidelity
screenshare (with its own audio), and webcam – over a peer-to-peer mesh,
coordinated by a small self-hosted Rust server. It is a persistent "always
available" hangout, not a federated platform.

**Design priorities, in order:** independent media flows (a video failure must
never drop voice) → low latency → high screenshare fidelity → simplicity of
self-hosting.

### In scope (MVP)

- Desktop: **Windows + Linux (X11)**. macOS only if it comes for free via the
  cross-platform engine. Mobile is a definite later phase (full, including
  phone screenshare) but explicitly **out of MVP**.
- Three media flows: microphone voice, screenshare + screen audio, webcam.
- Hardware-encoded screenshare with **OBS-style runtime encoder detection**
  across AMF / NVENC / QuickSync / VAAPI / VideoToolbox + software fallback.
  AMD (AMF/VAAPI) is the primary development/test target.
- Presence / online status.
- Text chat with attachments (images, files, audio/video playback).
- Push-to-talk, mute, deafen.
- Username + password auth (admin-provisioned accounts).
- **Stretch within MVP:** per-application screenshare-with-audio.

### Out of scope

- Local call/screen recording.
- Federation / multi-server.
- Public self-service registration.
- Video thumbnails for chat attachments (image thumbnails are in scope).
- Mobile (deferred to a post-MVP phase).

---

## 2. Subsystem Decomposition

Five independently-buildable subsystems, each to get its own spec → plan →
build cycle.

| # | Subsystem | Responsibility | Talks to |
|:--|:--|:--|:--|
| **S1** | Backend server (Rust/Axum) | Auth (JWT + bcrypt/argon2), accounts, presence, text chat, attachments API, WebSocket **signaling** (SDP/ICE relay) | Postgres, RustFS; all clients |
| **S2** | Media engine (Rust + GStreamer) | The 3 flows: capture → encode → `webrtcbin` → P2P; decode → Flutter textures; encoder capability detection | Peers (P2P); coturn |
| **S3** | Desktop client (Flutter + `flutter_rust_bridge`) | UI: presence, call view, screenshare picker, chat, PTT/mute/deafen; hosts S2 over FFI | S1 (WS + REST); S2 (FFI) |
| **S4** | TURN relay (coturn) | NAT traversal fallback when direct P2P fails | clients |
| **S5** | Infra / observability | Deploy, TLS, monitoring, secrets | S1, S4 |

### Boundaries that matter

- **S2 (media) is decoupled from S1 (signaling).** The server only brokers the
  handshake; once peers connect, media never touches the server. A server
  restart does not drop an active call's media.
- **S2 is decoupled from S3 (UI) across the FFI line.** Heavy encode/decode
  lives in Rust; the UI sends commands and receives texture handles, so UI
  navigation never stutters media. No media bytes cross into Dart.
- **The three media flows are independent within S2** (separate tracks), so a
  screen-encoder failure cannot take down voice.

---

## 3. Backend Server (S1)

A single Rust/Axum service structured in the **CLELO** layered style adapted to
Rust: **handlers → services → repositories → entities**, explicit
request/response DTO structs (no ad-hoc JSON), a `security` helper module,
Postgres via sqlx.

### Auth (mirrors CLELO)

- Username + password; passwords hashed with **argon2id** (current best
  practice; bcrypt is the direct CLELO parallel and an acceptable alternative).
- Login issues a **JWT** carrying `sub` (user id), `username`, `roles`
  (`USER` / `ADMIN`), `exp`. Stateless verification on every request.
- **No public registration.** An admin creates accounts / issues invite tokens
  (fits a trusted group). `/auth/login`, `/auth/me`; admin-only `/users`.
- Short-lived access JWT + a long-lived **refresh token stored server-side**
  (revocable) so a lost device can be cut off. (This adds revocation on top of
  CLELO's pure-JWT model, justified for a persistent personal tool.)

### Presence

Each client holds one authenticated **WebSocket**. Connect = online,
disconnect = offline. Server tracks `{user → status, current_room}` in memory
and broadcasts changes. No DB writes on the hot path; presence is ephemeral.

### Signaling

The same WebSocket carries the call handshake as a thin relay: `join_room`,
`offer`, `answer`, `ice_candidate`, `leave`, plus flow-level events (peer
started/stopped screenshare). The server **never parses media**; it routes
SDP/ICE blobs between peers. Rooms are named channels (likely one persistent
room for 3 friends).

### Text chat

Messages over the same WebSocket, persisted to Postgres. REST endpoint for
history/pagination on join. Deliberately minimal – no edits/reactions/threads
in MVP.

### Chat attachments (mirrors Lezio)

- **Object store: RustFS** (S3-compatible). Backend uses the internal endpoint;
  **presigned URLs** signed against a public endpoint so clients PUT/GET bytes
  directly – bytes never proxy through the server.
- **Two-phase upload:**
  1. `POST /attachments/initiate {filename, contentType, byteSize}` →
     `{attachmentId, key, uploadUrl, expiresIn}` (creates a `pending` row).
  2. Client `PUT`s bytes to `uploadUrl` with the file's `Content-Type`.
  3. `POST /attachments/:id/complete` → HEAD-verifies the object, flips to
     `ready`, triggers async media processing.
  4. `GET /attachments/:id/url` → `{url, contentType, byteSize, expiresIn}`.
- **ID-based rendering + caching:** messages carry only `attachmentId`. The
  client resolves the URL + type lazily, **keyed/cached by attachment id**
  (Flutter `FutureProvider.family`), so no message-schema or WebSocket changes;
  the same id renders from both REST history and live WS frames.
- **Renderers:** `image/*` inline (tap → fullscreen), `video/*` player,
  `audio/*` mini-player, else file chip.
- **Server-side media processing on `complete`** (beyond Lezio's current plan):
  - **Images:** compress + generate thumbnails. Rust equivalent of `sharp` is
    **`libvips`** (via `libvips-rust-bindings`; the `image` crate for simple
    cases). Original + compressed/thumbnail variant under the same id.
  - **Video:** light normalization / metadata (duration) via **`ffmpeg`**
    invoked as a subprocess. **No video thumbnails.**
  - Processing is async; the message stays usable while variants generate.
- Allowed types and a **100 MB** cap carried over from Lezio.

### Data model (Postgres 18, uuidv7 keys)

`users`, `refresh_tokens`, `rooms`, `room_members`, `messages`
(`attachment_id` nullable), `attachments(id, key, content_type, byte_size,
status, variants…)`. Presence stays in memory.

### Why one service, not microservices

At this scale, auth + presence + chat + signaling sharing one process and one
WebSocket per client is simpler, lower-latency, and easier to run. They split
into **modules**, not services.

---

## 4. Media Engine (S2)

Three independent flows, each a self-contained GStreamer pipeline, all
multiplexed onto **one WebRTC PeerConnection per peer** (mesh = 2 connections
for 3 people).

| Flow | Source | Codec | Behavior |
|:--|:--|:--|:--|
| **A – Voice** | Microphone (WASAPI / PipeWire) | Opus | Tight jitter buffer; no sync dependency; survives video failure |
| **B – Screen + audio** | Window/display + app/system audio | HEVC/AV1 (HW), Opus | High bitrate; screen A/V share one capture clock (auto lip-sync) |
| **C – Webcam** | Camera | H.264 / VP8, low bitrate | Cheap; optional per call |

### Capture backends (one Rust trait, per-OS impls)

- **Windows:** Graphics Capture (window/display) + WASAPI loopback (system) /
  **process loopback** (per-app audio).
- **Linux/X11:** X11/XComposite capture + **PipeWire** per-stream audio
  (enables per-app screenshare-with-audio).
- **macOS** (if free via GStreamer): ScreenCaptureKit + CoreAudio.
- Per-app screenshare-with-audio = window capture + that app's PipeWire stream
  (Linux) / process loopback (Windows). **Stretch**; falls back to
  full-display + system audio.

### Encoder selection (OBS-style)

At startup the engine probes available GStreamer encoder elements
(AMF → NVENC → QSV → VAAPI → VideoToolbox → software x265 / svt-av1) and picks
the best present. AMD/VAAPI is the primary test path. The active encoder is
surfaced to the UI for visibility/override.

### Transport – Approach A (chosen)

One PeerConnection per peer via `webrtcbin`; flows A/B/C are separate tracks
**BUNDLEd** over a single ICE/DTLS/SRTP transport → one coturn path, A/V sync
via RTCP sender reports.

**Stage-2 fallback (documented, not built):** if screen quality proves
insufficient under congestion, flow B graduates to a dedicated high-bitrate
UDP/QUIC transport with its own relay. Pay this complexity only if the M2/M4
quality gate demands it.

### Fault isolation

The screen encoder runs on its own thread (later: own process) so a GPU-encoder
crash degrades to "no screen" rather than dropping the call. Tracks are
independent SRTP streams, so loss/decoder errors on B never interrupt A.

### FFI surface to Flutter (`flutter_rust_bridge`)

- **Commands in:** `join_call`, `start_screenshare{source}`, `stop_flow`,
  `set_mute`, `select_encoder`.
- **Events out:** peer joined/left, flow state, active encoder, stats.
- **Texture handles** for decoded video, rendered directly by Flutter. No media
  bytes cross into Dart.

---

## 5. Infrastructure & Deployment (S5)

One small VPS = control plane. Media stays P2P and bypasses it.

| Component | Role |
|:--|:--|
| **Traefik** | Reverse proxy + automatic TLS (Let's Encrypt) for API/WS + admin |
| **Hearth server** (Rust/Axum) | Auth, presence, chat, signaling, attachments – one container |
| **Postgres 18** | Users, tokens, rooms, messages, attachment metadata (uuidv7) |
| **RustFS** | S3-compatible object store for attachment bytes |
| **coturn** | STUN/TURN NAT-traversal fallback (UDP/TCP + relay range) |
| **Grafana + Loki + Dozzle** | Metrics, log aggregation, container log viewing |

- **Config & secrets:** Docker Compose (`compose.dev.yml` / `compose.prod.yml`,
  Lezio-style); secrets via **sops + age**. `.env.example` documents `S3_*`,
  JWT secret, DB URL, TURN credentials.
- **Networking:** coturn needs its UDP relay range reachable; RustFS needs a
  **CORS policy** if clients ever PUT/GET directly from a web/mobile build.
  Server, DB, and RustFS-internal traffic stay on the Docker network; only
  Traefik, coturn, and the RustFS public endpoint are exposed.
- **Clients** ship as native Flutter desktop builds (Windows installer / Linux
  AppImage or package). No server-side hosting of the app itself.
- **Connectivity backbone:** standalone ICE + coturn. **Headscale** is demoted
  to an optional admin convenience / hard-NAT escape hatch, not a dependency.

---

## 6. Build Order, Milestones & Risks

Each milestone is independently demoable.

1. **M0 – Repo + skeleton:** `hearth/` monorepo, Docker compose, Postgres,
   empty Axum service, local run.
2. **M1 – Auth + presence:** login (JWT + argon2, CLELO-style layering),
   WebSocket presence, admin-created accounts.
3. **M2 – Media risk spike (make-or-break):** two desktop peers,
   hardware-encoded screenshare P2P via GStreamer + `webrtcbin` over manual
   signaling; measure latency + quality on AMD. **Go/no-go gate for Approach A.**
4. **M3 – Real signaling:** move the handshake into the Hearth server; join a
   room from the Flutter UI.
5. **M4 – All three flows:** add voice + webcam; PTT/mute/deafen; verify fault
   isolation (kill screen encoder, voice survives).
6. **M5 – Chat + attachments:** text chat, RustFS two-phase upload, id-based
   rendering, image compression/thumbnails.
7. **M6 – Deploy + observe:** Traefik/TLS, coturn on VPS, Grafana/Loki, sops.
8. **M7 – Polish:** per-app screenshare-with-audio (stretch), encoder picker UI,
   reconnection handling.
9. **Post-MVP:** mobile (extends S2/S3); Stage-2 dedicated screen transport
   only if the quality gate demands it.

### Top risks

- **R1 (highest): WebRTC throttles high-bitrate screen video.** Mitigation: M2
  spike validates early; Stage-2 transport documented as fallback.
- **R2: Cross-platform capture/encoder variance** (AMF vs VAAPI vs Windows).
  Mitigation: GStreamer abstraction + capability probe; AMD as primary test.
- **R3: `flutter_rust_bridge` ↔ GStreamer texture sharing** (zero-copy video
  into Flutter). Mitigation: prototype the texture path in M2/M3; fall back to a
  copy path if needed.
- **R4: NAT traversal on real friends' networks.** Mitigation: coturn relay;
  test from M3.

---

## 7. Reference Projects

- **CLELO backend** (`clelo/clelo_backend`): layered controller → repository →
  entity structure, JWT + bcrypt auth, request/response DTOs, security helper.
  The structural template for S1.
- **Lezio** (`lezio/`): RustFS two-phase upload, id-based attachment caching,
  Docker compose layout, sops + age secrets, Postgres 18 uuidv7.

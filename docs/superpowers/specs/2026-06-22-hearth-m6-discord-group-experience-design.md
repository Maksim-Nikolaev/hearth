# Hearth M6 – Discord-style Group Experience (Design)

_Status: design agreed 2026-06-22. Builds on the M5 desktop shell._

## Goal

Turn the 1:1 M5 shell into a Discord-style group app for a friend group (target:
5–8 gamers): a 3-pane layout (channels + self-panel | stage + chat | members), a
**voice channel** you join for **group voice over a P2P mesh**, and **multi-sharer
screenshare** where any member can stream their screen and each viewer watches one
stream at a time with an instant switch. Architected so a backend **SFU** can take
over screenshare fan-out later without touching the UI.

## North-star media topology

- **Voice:** P2P **full mesh**, always. Opus is tiny (~tens of kbps); even at 8
  people a 7-up mesh is fine.
- **Screenshare:** **P2P now** (local testing, 2–3 people: a sharer uploads one
  copy per viewer). **Backend SFU later** (production 5–8: sharer uploads once,
  server fans out) — high-bitrate screen × many viewers is too much to fan out
  P2P. The screenshare flow sits behind a **`ScreenTransport`** seam so the SFU is
  additive.
- **Backend never touches voice or P2P screenshare media** — control plane only,
  until the SFU milestone.

## Locked decisions

- Single **Voice** channel for now (structure supports adding more later).
- Chat sits **below the stage**, and takes the full center when nobody is sharing.
- **Glare rule (voice only):** voice is a bidirectional mesh where both sides want
  to connect, so the peer with the **smaller UUID is the offerer**; the other
  answers. Screenshare is inherently directional (the sharer always offers to each
  viewer), so it does not use this rule.
- Desktop `app.rs` is **split into relm4 sub-components** (it has grown too large).
- **In scope:** 3-pane layout, group voice mesh, multi-sharer P2P screenshare +
  selectable stage, voice join/leave, dark theme.
- **Out of scope (next):** the **SFU** (seam ready), multiple voice channels,
  webcam, mobile.

## Layout

```
┌───────────────┬──────────────────────────────────┬───────────────┐
│ CHANNELS      │  STAGE (selected screenshare)     │  MEMBERS      │
│  TEXT         │  ┌────────────────────────────┐   │  IN VOICE     │
│   # general   │  │   [ bob's screen ]         │   │   🔊 bob      │
│  VOICE        │  └────────────────────────────┘   │   🔊 jimmy    │
│   🔊 Voice    │  Watching: ◉ bob  ○ jimmy          │  ONLINE       │
│     🔊 bob    │  ─────────────────────────────     │   ● alice(you)│
│     🔊 jimmy  │  chat: # general + input          │  OFFLINE      │
├───────────────┤                                   │   ○ sam       │
│ alice ●       │                                   │               │
│ [Mute][Deafen]│                                   │               │
│ [Share]       │                                   │               │
└───────────────┴──────────────────────────────────┴───────────────┘
```

- **Left rail:** text channels; a **Voice** channel listing who is in it (click to
  join → voice-connect to everyone there). Bottom: **self-panel** (mute / deafen /
  share toggle).
- **Center stage:** the selected screenshare `gtk::Picture` + a **Watching**
  switcher over active sharers (instant – just swaps which received paintable is
  shown). Chat for the current text channel below; chat fills the center when no
  one is sharing.
- **Right:** members grouped **In Voice / Online / Offline**.

## Protocol additions (`hearth-protocol`)

```rust
// ClientMessage
VoiceJoin,
VoiceLeave,
ShareStart,
ShareStop,

// ServerMessage
VoiceState  { members: Vec<PeerInfo> },   // roster, sent on VoiceJoin
VoiceJoined { user: Uuid, username: String },
VoiceLeft   { user: Uuid },
ShareStarted { user: Uuid },
ShareStopped { user: Uuid },
```

Existing `Offer/Answer/Ice{flow}` and chat are unchanged. The backend relays all
of this opaquely (still no SDP parsing).

## Backend

- **Voice sub-room** in the signaling hub, parallel to the room: track voice
  membership; on `VoiceJoin` send `VoiceState` to the joiner and `VoiceJoined` to
  the others; on `VoiceLeave`/disconnect send `VoiceLeft`.
- **Share relay:** `ShareStart`/`ShareStop` broadcast to current voice members as
  `ShareStarted`/`ShareStopped`.
- Tests mirror the existing presence/chat integration tests (two clients: join
  voice → roster + notify; share → broadcast).

## Engine / Session

- **Voice membership:** `Session` tracks the voice roster. `join_voice()` sends
  `VoiceJoin`; on `VoiceState`/`VoiceJoined`, for each other member open a **Voice
  `FlowPeer`**, using the glare rule to pick offerer vs answerer (so exactly one
  side offers). On `VoiceLeft`, `stop_flow(peer, Voice)`. `leave_voice()` tears
  down all Voice flows and sends `VoiceLeave`.
- **Offerer rule:** a helper `should_offer(self_id, peer_id) = self_id < peer_id`.
  The smaller-UUID side creates the offerer `FlowPeer`; the larger side waits for
  the offer (creates an answerer on receipt — already handled by `handle`).
- **Screenshare:** `start_share()` sends `ShareStart` and opens a **Screen
  `FlowPeer` (offerer) to each voice member**; `stop_share()` sends `ShareStop`
  and stops those flows. Incoming shares are answered as today (replace-on-offer).
- **`ScreenTransport` seam:** the Screen flow's create-offer/answer path is behind
  a trait with a `P2pTransport` impl now; a future `SfuTransport` negotiates with
  the backend instead of per-peer. Voice is always mesh (no seam).
- **Stage data:** `SessionEvent` already carries `VideoReady { peer, flow }` and
  `FlowState`; add `SessionEvent::ShareStarted { user }` / `ShareStopped { user }`
  (from the backend relay) so the UI maintains the sharer set for the Watching
  switcher; `paintable_for(peer, Screen)` feeds the stage.

## Desktop UI

Split the monolith into relm4 components, each with one responsibility:

```
desktop/src/
  app.rs              root shell: owns Session, routes Login <-> Workspace
  ui/login.rs         (extracted from app.rs)
  ui/workspace.rs     the 3-pane container
  ui/channels.rs      text + voice channel list + join voice
  ui/self_panel.rs    your name + mute / deafen / share toggle
  ui/stage.rs         gtk::Picture + Watching switcher + chat
  ui/members.rs       In Voice / Online / Offline (FactoryVecDeque)
  ui/chat.rs          message list (FactoryVecDeque) + entry
  theme.rs            dark Discord-like GTK CSS
```

State the UI tracks: channels, voice roster, sharers, selected stage stream,
messages, online roster. Sub-components communicate with the root via relm4
messages; the root forwards `Session` ops and fans `SessionEvent`s out to them.

## Data flows

- **Join voice:** click Voice → `join_voice()` → `VoiceState` → mesh Voice flows to
  all members (offerer per UUID rule) → audio flows; member list shows them under
  In Voice.
- **Someone joins voice:** `VoiceJoined` → open a Voice flow to them (offerer iff
  smaller UUID).
- **Share:** toggle Share → `ShareStart` + Screen offer to each voice member →
  they render; sharer appears in everyone's Watching switcher.
- **Switch stage:** click another sharer in Watching → swap the `gtk::Picture`
  paintable to that peer's already-received Screen stream (instant).
- **Stop share / leave voice:** `ShareStop` / `VoiceLeave` tear down the relevant
  flows; others update their switcher / roster.

## Testing

- **Backend:** voice membership (roster + join/leave notify) and share-relay
  integration tests.
- **Engine:** `should_offer` unit test; group-routing unit tests (a `VoiceJoined`
  for a smaller/larger UUID creates an offerer/answerer respectively) via the mock
  WS, media assertions `#[ignore]`d as in M5.
- **Run-and-observe:** 2–3 instances — join voice (mutual audio), two members
  share at once, switch the stage between them, stop one share, leave voice; chat
  and the other flows keep running. Recorded in `desktop/README.md`.

## Risks

- **Mesh glare / renegotiation** – the UUID offerer rule is the main correctness
  hinge; covered by a unit test and the run-and-observe.
- **Audio device echo** on one machine with shared mic/speakers (expected; mute to
  test). Real devices per machine in normal use.
- **relm4 component split** – moving from one component to several is the bulk of
  the UI work; FactoryVecDeque for the dynamic lists.
- **`ScreenTransport` shape** – keep the trait minimal (create/accept a screen
  session for a target) so the future SFU impl is a clean drop-in.

# Two-instance voice test (dev)

Manual check that voice connects and carries audio between two local instances.
**Voice only — do NOT click "Share screen"** (that would capture your `:0`
desktop and can destabilise other apps). Run each block from the repo root
(`~/Desktop/work/hearth`).

## 1. Build the client

```bash
cd ~/Desktop/work/hearth
. "$HOME/.cargo/env"
cargo build -p desktop
```

## 2. Start Postgres + backend

Skip if they are already running. Leave the backend running in its own terminal
(or background it with `&`).

```bash
cd ~/Desktop/work/hearth
docker compose -f compose.dev.yml up -d postgres
```

```bash
cd ~/Desktop/work/hearth
. "$HOME/.cargo/env"
DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth \
JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret \
cargo run -p hearth-backend
```

Wait for `hearth-backend listening on 0.0.0.0:8080`.

## 3. Mint tokens (valid ~15 min)

Uses a small script so there is nothing to mis-paste:

```bash
cd ~/Desktop/work/hearth
bash scripts/dev/mint-tokens.sh
```

Expected: `minted: alice=… bytes, bob=… bytes`. Re-run this if a launch later
shows "Login failed: 401" (the token expired).

## 4. Launch the two instances

```bash
cd ~/Desktop/work/hearth
DISPLAY=:0 HEARTH_TITLE=alice HEARTH_TOKEN="$(cat /tmp/alice.token)" ./target/debug/desktop > /tmp/v-alice.log 2>&1 &
DISPLAY=:0 HEARTH_TITLE=bob   HEARTH_TOKEN="$(cat /tmp/bob.token)"   ./target/debug/desktop > /tmp/v-bob.log 2>&1 &
```

## 5. Verify

In **both** windows, click the **🔊 Voice (join)** entry in the left rail.

- **Both move to "IN VOICE"** in the right rail → signaling works.
- **Audio passes**: speak into your mic; you should hear it on the other
  instance. On one machine both share your mic+speakers, so **mute one instance
  first** (or use headphones) to avoid echo/howl.
- **Mute** silences your transmission.

Log sanity check (run after joining):

```bash
grep -c "incoming voice linked" /tmp/v-alice.log /tmp/v-bob.log   # expect 1 each
grep -iE "error|panic" /tmp/v-alice.log /tmp/v-bob.log | grep -vi gupnp | head
```

## 6. Tear down

```bash
pkill -x desktop            # close both client windows
# stop the backend with Ctrl-C in its terminal (or: pkill -f hearth-backend)
```

## What this exercises

The M7 single-capture voice path: one mic capture → DSP (echo cancel / noise
suppression / AGC / VAD) → activation gate → fan-out to each peer. If voice
connects and is audible here, the rewired capture pipeline is sound. DSP toggles
and device selection get their own UI in later tasks.

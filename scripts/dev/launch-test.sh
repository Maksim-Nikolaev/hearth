#!/usr/bin/env bash
# Launch a two-window local test setup (alice + bob) against the dev backend.
#
# Default:    build the desktop binary (debug), mint tokens, launch both windows.
#   --release build and launch the optimised release binary (representative for
#             screenshare latency / "max performance" testing; slower to build).
#   --debug   skip the build and launch the already-built binary as-is. Combines
#             with --release to launch the existing release binary without rebuilding.
#   --synthetic
#             feed a synthetic videotestsrc to screenshare/preview instead of the
#             real screen (safe for UI testing; avoids grabbing the real :0).
#   --fresh   wipe each window's isolated config first, so alice and bob start with
#             empty settings (useful for the latency rig: each can pick its own
#             audio devices). Configs are always isolated per window under
#             /tmp/hearth-test/<name>; --fresh just empties them.
#
# The backend must already be running on :8080 (see docs/dev/voice-test.md):
#   docker compose -f compose.dev.yml up -d postgres
#   cargo run -p hearth-backend
#
# Usage:
#   scripts/dev/launch-test.sh              # build, then launch alice + bob
#   scripts/dev/launch-test.sh --debug      # skip build, just launch
#   scripts/dev/launch-test.sh --synthetic  # build + launch with a fake screen
set -euo pipefail

cd "$(dirname "$0")/../.."

SKIP_BUILD=false
SYNTHETIC=false
RELEASE=false
FRESH=false
for arg in "$@"; do
  case "$arg" in
    --debug)     SKIP_BUILD=true ;;
    --release)   RELEASE=true ;;
    --synthetic) SYNTHETIC=true ;;
    --fresh)     FRESH=true ;;
    -h|--help)   sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "unknown flag: $arg (try --help)" >&2; exit 2 ;;
  esac
done

export DISPLAY="${DISPLAY:-:0}"
# Local two-window test always targets the local dev backend. The desktop's
# server is set in the login box now, so we don't read HEARTH_HTTP (a stale
# export from a LAN test must not redirect this local run).
BASE="http://localhost:8080"
if [ "$RELEASE" = true ]; then
  BIN="./target/release/desktop"
else
  BIN="./target/debug/desktop"
fi

# 1. Backend reachability — mint-tokens and login both need it.
if ! (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null; then
  echo "✗ backend not reachable on :8080." >&2
  echo "  start it first:" >&2
  echo "    docker compose -f compose.dev.yml up -d postgres" >&2
  echo "    cargo run -p hearth-backend" >&2
  exit 1
fi
echo "✓ backend up on $BASE"

# 2. Build (unless --debug).
if [ "$SKIP_BUILD" = true ]; then
  if [ ! -x "$BIN" ]; then
    echo "✗ --debug given but $BIN does not exist; run without --debug once to build it." >&2
    exit 1
  fi
  echo "• skipping build (--debug), using existing $BIN"
else
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env" 2>/dev/null || true
  if [ "$RELEASE" = true ]; then
    echo "• building desktop (release)…"
    cargo build -p desktop --release
  else
    echo "• building desktop (debug)…"
    cargo build -p desktop
  fi
fi

# 3. Mint fresh tokens for alice + bob.
echo "• minting tokens…"
bash scripts/dev/mint-tokens.sh

# 4. Stop any prior test windows so we start clean.
pkill -x desktop 2>/dev/null || true
sleep 0.5

# 5. Optional synthetic capture (safe screenshare/preview without the real screen).
if [ "$SYNTHETIC" = true ]; then
  export HEARTH_CAPTURE="videotestsrc is-live=true pattern=ball ! timeoverlay ! videoconvert"
  echo "• synthetic capture enabled (HEARTH_CAPTURE set)"
fi

# 6. Launch both windows, each logging to /tmp.
launch() {
  local name="$1" log="/tmp/hearth-$1.log"
  local cfg="/tmp/hearth-test/$name"

  # Isolated config per window so alice and bob don't share settings (e.g. each
  # can select its own audio devices). --fresh empties it for a clean start.
  if [ "$FRESH" = true ]; then
    rm -rf "$cfg"
  fi
  mkdir -p "$cfg"

  HEARTH_TITLE="$name" HEARTH_TOKEN="$(cat "/tmp/$name.token")" HEARTH_CONFIG_DIR="$cfg" \
    "$BIN" >"$log" 2>&1 &
  echo "  $name → pid $! (cfg: $cfg, log: $log)"
}

echo "• launching windows on $DISPLAY…"
launch alice
launch bob
disown -a

echo
echo "✓ alice + bob running. Tail logs with:  tail -f /tmp/hearth-alice.log"
echo "  stop both with:                       pkill -x desktop"

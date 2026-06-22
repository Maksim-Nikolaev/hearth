#!/usr/bin/env bash
# Mint short-lived access tokens for the dev users alice + bob into /tmp.
# Usage: bash scripts/dev/mint-tokens.sh   (backend must be running on :8080)
set -euo pipefail

BASE="${HEARTH_HTTP:-http://localhost:8080}"

for u in alice bob; do
  # Heredoc body is NOT indented on purpose, so Python never sees stray
  # leading whitespace (which caused the IndentationError when pasted inline).
  python3 - "$u" "$BASE" <<'PY' > "/tmp/$u.token"
import json, sys, urllib.request

user, base = sys.argv[1], sys.argv[2]
body = json.dumps({"username": user, "password": f"pw-{user}"}).encode()
req = urllib.request.Request(
    f"{base}/auth/login",
    data=body,
    headers={"Content-Type": "application/json"},
)
print(json.load(urllib.request.urlopen(req))["access_token"])
PY
done

echo "minted: alice=$(wc -c </tmp/alice.token) bytes, bob=$(wc -c </tmp/bob.token) bytes"

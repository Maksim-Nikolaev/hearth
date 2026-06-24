#!/usr/bin/env bash
# Launcher for audio_delay.py on Linux. Drag-and-drop one or more .mkv files
# onto "Measure audio delay.desktop", or run this from a terminal:
#   ./audio_delay.sh clip1.mkv clip2.mkv ...
#
# numpy/scipy live in a private venv under the user cache dir, created on first
# run so the launcher needs no system packages and no sudo.
set -euo pipefail

DIR="$(dirname "$(readlink -f "$0")")"
VENV="${XDG_CACHE_HOME:-$HOME/.cache}/hearth-audio-delay/venv"
PY="$VENV/bin/python"

if [ ! -x "$PY" ]; then
  echo "First run: creating analysis environment in $VENV ..."
  python3 -m venv "$VENV"
  "$PY" -m pip install --quiet --upgrade pip
  "$PY" -m pip install --quiet numpy scipy
fi

if [ "$#" -eq 0 ]; then
  echo "Drag one or more .mkv recordings onto the launcher, or pass them as arguments."
else
  "$PY" "$DIR/audio_delay.py" "$@"
fi

echo
read -rp "Done. Press Enter to close..."

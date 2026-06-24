#!/usr/bin/env bash
# Launcher for audio_delay.py on Linux.
# Self-locates audio_delay.py in the same directory, runs it on the files
# passed by the .desktop launcher (or on the command line), then pauses.
DIR="$(dirname "$(readlink -f "$0")")"
python3 "$DIR/audio_delay.py" "$@"
echo
read -rp "Done. Press Enter to close..."

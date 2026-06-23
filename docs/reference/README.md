# Reference source dumps

Whole-repo source dumps of two projects we benchmark against, kept in-tree for
offline reference (and for future AI sessions to grep). They are **reference
only** — not part of the build, not our code, their own licenses apply.

- **`obs-studio-source.txt`** — [OBS Studio](https://github.com/obsproject/obs-studio)
  (C++). The benchmark for capture/encode quality and flexibility. Most useful
  for: per-source capture (display/window/game), **application audio capture**
  (WASAPI process loopback on Windows; PipeWire on Linux), Windows Graphics
  Capture (WGC) / DXGI, and — importantly for us — the **Wayland/Linux** capture
  paths (PipeWire portal, screencast).
- **`vesktop-source.txt`** — [Vesktop](https://github.com/Vencord/Vesktop)
  (Electron/JS). A custom Discord client; useful for how it does Linux screenshare
  (incl. Wayland) and audio capture from Electron.

Why they're here: the owner wants OBS-level screenshare and is targeting
**Wayland Linux** later (the repo's recent history was an X11→Wayland→Windows
trajectory). When we build the Wayland capture path and the OBS-style per-source
A/V model (see `docs/VISION.md`), these are the canonical references.

To search: `rg "process.loopback" docs/reference/obs-studio-source.txt`, etc.

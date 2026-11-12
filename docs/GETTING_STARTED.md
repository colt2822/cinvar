# Getting started with CINVAR (developer beta)

> **Honest status:** CINVAR is a command-line tool plus an MCP server for AI assistants. There is **no graphical
> installer and no desktop editor**. Someone who has never opened a terminal cannot use it yet; a GUI/installer is a
> listed beta requirement. This guide is for people comfortable running a few commands.

## 1. Install
```
git clone <your copy of the repository> && cd cinvar
./scripts/install.sh            # or build yourself: cargo build --release
cinvar doctor                   # shows what is installed: FFmpeg, encoders, Blender, fonts
```
You need FFmpeg + ffprobe on PATH (`sudo apt install ffmpeg`, `brew install ffmpeg`, `choco install ffmpeg`).
Windows and macOS builds are prepared in CI (`.github/workflows/ci.yml`) but have **not been executed yet**.

## 2. First project (no accounts, no cost)
```
cinvar project create --name "My Film" --dir ~/Videos
cinvar --project ~/Videos/my-film run "Combine the clips in ~/Footage, add a title and export 1080p"   # needs a model profile; see `cinvar models`
```
Or connect an assistant: `cinvar connect claude --project ~/Videos/my-film`.

## 3. Presets and limits
`cinvar.render.preflight` (via MCP) answers "can this machine do 4K / 8K?" before you start. Presets: 720p, 1080p,
1440p, 4k, 8k. On a modest laptop stay at 1080p; 8K needs roughly 6-10 GB RAM and a fast CPU encoder.

## 4. Optional integrations (each is opt-in)
| Want | Do |
|---|---|
| 3D titles / Blender scenes | install Blender 3.6+ or set `CINVAR_BLENDER` |
| Subtitles from speech | `pip install faster-whisper` in a venv (PyAV < 17); set `CINVAR_FASTER_WHISPER_PYTHON`, `CINVAR_FASTER_WHISPER_MODEL` (a local model) |
| Colour transforms | `pip install opencolorio` in a venv; set `CINVAR_OCIO_PYTHON` |
| Cloud rendering | `cinvar cloud connect` (paste your RunPod key), set `security.allow_cloud_render = true`; read the quote, then approve it yourself: `cinvar cloud approve <quote-id>`. **Not tested against live RunPod.** |
| Editor interchange | `cinvar otio export` / `cinvar.otio.import` |
Plugin status and install hints: MCP tool `cinvar.plugins.list`.

## 5. Troubleshooting
| Symptom | Fix |
|---|---|
| `ffmpeg not found` | install FFmpeg and re-run `cinvar doctor` |
| 8K export "preflight failed" | the message states the missing RAM/disk; use a lower preset |
| `h264_nvenc` fails at 8K | expected on some GPUs; CINVAR falls back to a CPU encoder automatically |
| Cloud tool says "disabled" | set `security.allow_cloud_render = true`; it is off by default |
| "outside the project" | agents may only read/write inside the project. Import footage yourself (`cinvar media import FILE`) or allow a folder: `--media-root DIR` / `CINVAR_MEDIA_ROOTS` |
| Import says "not importable as media" | the file's *content* is not a supported media container |
| faster-whisper `metadata_errors` TypeError | install `av<17` in the same venv |
| Plugin command refused: "no sandbox" | install bubblewrap (Linux). Windows/macOS have no sandbox yet; `security.allow_unsandboxed_plugins = true` runs plugins with your full privileges |

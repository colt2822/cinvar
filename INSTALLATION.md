# Installing CINVAR

CINVAR is a single Rust binary plus FFmpeg. There is no server to run, no database to provision and
no account to create.

## Requirements

| Requirement | Needed for | Notes |
|---|---|---|
| FFmpeg + ffprobe (5.1 or newer, with libfreetype) | everything | the only hard runtime dependency; drawtext needs a full build |
| Rust 1.85+ | building from source | not needed if you use a release build |
| Python 3.10+ | `cinvar setup --pro-ai` | creates the per-component virtualenvs |
| Blender 3.x+ | optional 3D animation | detected automatically, never required |
| A model endpoint | conversational editing | can be a cloud API or a local server |

Linux x86_64 is the platform verified in this repository. macOS and Windows are supported by the
code paths (path handling, subprocess, CI matrix) but have **not** been verified by running the full
demo suite here - treat them as untested, not as broken. Windows has its own notes in
`docs/WINDOWS_DISTRIBUTION.md`.

## Install FFmpeg

    Debian/Ubuntu   sudo apt install ffmpeg
    Fedora          sudo dnf install ffmpeg
    macOS           brew install homebrew-ffmpeg/ffmpeg/ffmpeg --with-freetype
    Windows         winget install Gyan.FFmpeg     (choose the "full" build, not "essentials")

A build without libfreetype has no `drawtext` filter: titles and burned-in captions then fail with
"this FFmpeg build has no 'drawtext' filter". `cinvar doctor` reports `drawtext_filter`.

## Install a prebuilt build

Every release artifact is published next to its SHA-256. The installer verifies the checksum
before it touches anything, refuses plain `http`, installs into `~/.local` without sudo, and then
runs `cinvar doctor`:

    curl -fLO https://example.invalid/cinvar-0.2.0-linux-x86_64.tar.gz
    curl -fLO https://example.invalid/cinvar-0.2.0-linux-x86_64.tar.gz.sha256
    sha256sum -c cinvar-0.2.0-linux-x86_64.tar.gz.sha256
    ./scripts/install-prebuilt.sh cinvar-0.2.0-linux-x86_64.tar.gz        # add --pro-ai to continue

    # or, straight from a URL (the sibling .sha256 is fetched automatically):
    ./scripts/install-prebuilt.sh https://example.invalid/cinvar-0.2.0-linux-x86_64.tar.gz

Other formats:

* **AppImage** (single file, no install): `chmod +x cinvar-0.2.0-x86_64.AppImage` then run it.
* **Homebrew**: `brew install --formula ./Formula/cinvar.rb`, rendered from published artifacts by
  `scripts/render_homebrew_formula.sh`.
* **Windows**: unzip `cinvar-<version>-windows-x86_64.zip`, then follow
  `docs/WINDOWS_DISTRIBUTION.md` (config paths and the font rule differ on Windows).

## Install from source

From a checkout (replace the URL with wherever you got the source):

    git clone <repository> && cd cinvar
    ./scripts/install.sh          # Linux / macOS: builds with cargo, installs to ~/.local/bin, no sudo
    .\scripts\install.ps1         # Windows (PowerShell)

Or build directly:

    cargo build --release
    ./target/release/cinvar doctor

Packaging a release build (reproducible tarball, AppImage, formula, Windows zip) is documented in
`docs/PACKAGING.md`.

## Pro AI stack: one command

WhisperX, faster-whisper, Real-ESRGAN and Demucs each need a Python environment. `setup --pro-ai`
creates them and records where everything landed, so no `CINVAR_*_PYTHON` variable has to be
exported by hand:

    cinvar setup --pro-ai            # add --json for machine-readable output

What it does, per component, under one managed root:

* one virtualenv per component in `~/.local/share/cinvar/venvs/<component>/` (reused on re-run);
* a **pinned torch stack** (torch, torchaudio, torchvision from one CUDA channel: `2.8.0+cu128`
  when `nvidia-smi` reports a GPU, `2.8.0+cpu` otherwise; override with
  `CINVAR_PRO_AI_DEVICE=cpu|cuda`). The pair is installed in a single pip pass and **verified by
  importing both wheels** before the component is reported ready - a torchaudio wheel from another
  CUDA channel aborts at import and is the failure this pin exists to prevent;
* the `libomp.so.5` shim Real-ESRGAN's ncnn build needs, as `<venv>/lib-shim/libomp.so.5` pointing
  at the system `libgomp.so.1` (the adapter prepends that directory to `LD_LIBRARY_PATH`);
* only the model weights that component actually loads, in
  `/mnt/elyreon-data/cinvar-models/<component>/` when that mount exists, else
  `~/.local/share/cinvar/models/<component>/`. Existing weights are reused;
* one line per component on stdout (or JSON), with progress on stderr.

A component that fails reports its failing stage, the last output lines, the command and the next
step, and the remaining components still install. Full per-stage logs are written to
`~/.local/share/cinvar/logs/setup-<component>.log`. Re-running resumes from the pip cache.

The resolved paths are written to `~/.config/cinvar/pro-ai.toml`, and the equivalent environment is
printed at the end of the run:

    [1/4] installing torch 2.8.0+cu128 + torchaudio 2.8.0+cu128 from https://download.pytorch.org/whl/cu128
    ...
    [2/4] whisperx: torch/torchaudio import ok: 2.8.0+cu128 2.8.0+cu128

## First run

    cinvar setup --yes    # checks dependencies, writes a starter configuration
    cinvar doctor         # shows exactly what is detected on this machine
    cinvar models add local --provider ollama --model qwen2.5:7b
    cinvar models use local
    cinvar models test local

Offline, without any credentials, use the bundled planner:

    cinvar models add mock --provider mock --model mock-planner
    cinvar models use mock

## Fastest first video with Claude Code

    cinvar connect claude --project ./my-video --footage ~/Videos/shoot-1
    cd my-video
    claude        # approve the 'cinvar' server, then: "Make a 30 second cinematic trailer from my footage."

## Create the first project

    cinvar project create --name MyVideo
    cd my-video
    cinvar run "Combine the clips in ~/footage, add a title, and export a 1080p video"

Or use the terminal conversation:

    cinvar chat

Or the local web UI (no cloud, bound to 127.0.0.1):

    cinvar ui --port 8420

## Verify the installation

    cinvar doctor                  # FFmpeg, drawtext, Blender, model endpoint, config path
    cargo test                     # unit + integration tests, including a real ffprobe-validated render
    ./examples/demo1_basic_editing.sh
    ./scripts/acceptance.sh        # full customer path: setup -> auto-edit -> QA -> export

## Uninstall

    rm -f ~/.local/bin/cinvar
    rm -rf ~/.config/cinvar ~/.local/share/cinvar      # config, venvs, logs, cached models
    rm -rf /mnt/elyreon-data/cinvar-models             # only if setup used the data mount

Projects are ordinary directories; delete the project directory to delete the project.

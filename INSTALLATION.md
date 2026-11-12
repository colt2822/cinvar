# Installing CINVAR

CINVAR is a single Rust binary plus FFmpeg. There is no server to run, no database to
provision and no account to create.

## Requirements

| Requirement | Needed for | Notes |
|---|---|---|
| FFmpeg + ffprobe (5.1 or newer) | everything | the only hard runtime dependency |
| Rust 1.85+ | building from source | not needed if you use a release binary |
| Blender 3.x+ | optional 3D animation | detected automatically, never required |
| A model endpoint | conversational editing | can be a cloud API or a local server |

Linux is the platform verified in this repository. macOS and Windows are supported by the
code paths (path handling, subprocess, CI matrix) but have **not** been verified by running
the full demo suite here - treat them as untested, not as broken.

## Install FFmpeg

    Debian/Ubuntu   sudo apt install ffmpeg
    Fedora          sudo dnf install ffmpeg
    macOS           brew install ffmpeg
    Windows         winget install Gyan.FFmpeg

## Install CINVAR

From a checkout (replace the URL with wherever you got the source):

    git clone <repository> && cd cinvar
    ./scripts/install.sh          # Linux / macOS: builds with cargo, installs to ~/.local/bin, no sudo
    .\scripts\install.ps1         # Windows (PowerShell) - NOT TESTED

No prebuilt binaries are published yet. `scripts/install.sh` is tested on Linux only.

Build from source directly:

    cargo build --release
    ./target/release/cinvar doctor

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

    cargo test                     # unit + integration tests, including a real ffprobe-validated render
    ./examples/demo1_basic_editing.sh

## Uninstall

    rm -f ~/.local/bin/cinvar
    rm -rf ~/.config/cinvar ~/.local/share/cinvar

Projects are ordinary directories; delete the project directory to delete the project.

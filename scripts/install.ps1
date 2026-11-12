# Build CINVAR from this checkout and install it (Windows, PowerShell 5+). No administrator rights.
# STATUS: NOT TESTED on Windows - written but never executed. Please report problems.
#   git clone <repository>; cd cinvar; .\scripts\install.ps1
param([string]$Dest = "$env:LOCALAPPDATA\CINVAR")

$ErrorActionPreference = "Stop"
function Say($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Fail($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

$here = Split-Path -Parent $PSScriptRoot
if (-not (Test-Path (Join-Path $here "Cargo.toml"))) { Fail "run this script from a CINVAR checkout" }
if (-not ((Get-Command ffmpeg -ErrorAction SilentlyContinue) -and (Get-Command ffprobe -ErrorAction SilentlyContinue))) {
  Say "FFmpeg + ffprobe were not found. Install with: winget install Gyan.FFmpeg"
  Fail "FFmpeg is required for media work"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Fail "cargo is required (https://rustup.rs)" }

Push-Location $here
try { cargo build --release --locked } finally { Pop-Location }
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Copy-Item (Join-Path $here "target\release\cinvar.exe") (Join-Path $Dest "cinvar.exe") -Force
Say "installed to $Dest\cinvar.exe"
Say "add this to your PATH: $Dest"
& (Join-Path $Dest "cinvar.exe") doctor
Say "done. next: cinvar setup --yes"

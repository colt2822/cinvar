# Build the Windows CINVAR distribution: dist\cinvar-<version>-windows-<arch>.zip (+ SHA256SUMS).
#
# Run ON Windows (or a windows-latest CI runner): the binary needs the MSVC toolchain, and this
# repository's release binaries are not cross-compiled from Linux (see docs/PACKAGING.md).
#
#   powershell -ExecutionPolicy Bypass -File scripts\package_windows.ps1
#
# Requires: Visual Studio Build Tools (MSVC + Windows SDK), Rust (msvc toolchain), FFmpeg on PATH
# only for the smoke test (the archive does not bundle it).
#
# Env: CARGO_TARGET_DIR (default %TEMP%\tgt-package-windows), DIST (default dist),
#      CINVAR_ARCH (default x86_64 or aarch64 from the host).
param(
    [string]$CargoTargetDir = $env:CARGO_TARGET_DIR,
    [string]$Dist = "dist"
)
$ErrorActionPreference = "Stop"

function Say($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Fail($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Fail "cargo is required (install Rust from https://rustup.rs)" }
if (-not $CargoTargetDir) { $CargoTargetDir = Join-Path $env:TEMP "tgt-package-windows" }
$env:CARGO_TARGET_DIR = $CargoTargetDir

$version = ((Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"(.*)"').Matches[0].Groups[1].Value)
if (-not $version) { Fail "cannot read the version from Cargo.toml" }
$arch = if ($env:CINVAR_ARCH) { $env:CINVAR_ARCH } elseif ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$name = "cinvar-$version-windows-$arch"

Say "building release binary ($CargoTargetDir)"
cargo build --release --locked --bin cinvar
$bin = Join-Path $CargoTargetDir "release\cinvar.exe"
if (-not (Test-Path $bin)) { Fail "no binary at $bin" }

Say "assembling $name"
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("cinvar-stage-" + [guid]::NewGuid())
$out = Join-Path $stage $name
New-Item -ItemType Directory -Path (Join-Path $out "docs"), (Join-Path $out "examples"), (Join-Path $out "packaging") -Force | Out-Null
Copy-Item $bin (Join-Path $out "cinvar.exe")
foreach ($doc in @("LICENSE","NOTICE","THIRD_PARTY_NOTICES.md","README.md","INSTALLATION.md","SECURITY.md","CHANGELOG.md")) {
    if (Test-Path $doc) { Copy-Item $doc (Join-Path $out $doc) }
}
if (Test-Path "plugins") { Copy-Item "plugins" (Join-Path $out "plugins") -Recurse }
if (Test-Path "packaging\windows") { Copy-Item "packaging\windows\*" (Join-Path $out "packaging") -Recurse }
Get-ChildItem "docs\*.md" | Copy-Item -Destination (Join-Path $out "docs")
if (Test-Path (Join-Path $out "docs\WINDOWS_DISTRIBUTION.md")) {
    Copy-Item (Join-Path $out "docs\WINDOWS_DISTRIBUTION.md") (Join-Path $out "INSTALL-WINDOWS.md")
}

Say "smoke test"
& (Join-Path $out "cinvar.exe") --version | Out-Host
if ($LASTEXITCODE -ne 0) { Fail "the packaged binary does not run" }

Say "zipping"
New-Item -ItemType Directory -Path $Dist -Force | Out-Null
$zip = Join-Path $Dist ("$name.zip")
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $out -DestinationPath $zip -CompressionLevel Optimal
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content -Path ("$zip.sha256") -Encoding ascii -NoNewline
Say "sha256 $hash"

Say "verifying the archive"
$verify = Join-Path $stage "verify"
Expand-Archive -Path $zip -DestinationPath $verify
& (Join-Path $verify "$name\cinvar.exe") --version | Out-Host
if ($LASTEXITCODE -ne 0) { Fail "the extracted archive does not run" }
Remove-Item $stage -Recurse -Force

Write-Host ""
Write-Host "artifact: $zip"
Write-Host "checksum: $zip.sha256"

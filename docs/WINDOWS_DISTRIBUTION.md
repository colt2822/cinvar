# CINVAR on Windows

Windows is a supported build target, but it is **not built from this Linux checkout**. This page is
the recipe plus the configuration differences that actually matter on Windows.

## What a Windows distribution contains

`
cinvar-<version>-windows-<arch>.zip
  cinvar.exe
  INSTALL-WINDOWS.md      (this file)
  INSTALLATION.md  README.md  LICENSE  NOTICE  THIRD_PARTY_NOTICES.md  CHANGELOG.md  SECURITY.md
  docs\*                  (all documentation)
  packaging\*             (launcher scripts, icons)
`

The SHA-256 is written next to the archive, not inside it. Not included: FFmpeg (install it
separately, see below), Python environments or model weights (those come from
`cinvar setup --pro-ai`), the `pro` module, issuer keys.

## Build recipe

Run on a Windows machine or a `windows-latest` CI runner:

    # 1. Toolchain: Visual Studio Build Tools 2019/2022 with "Desktop development with C++"
    #    (MSVC + Windows SDK), then Rust's MSVC toolchain:
    rustup default stable-msvc

    # 2. FFmpeg full build (libfreetype for drawtext; "essentials" is not enough):
    winget install Gyan.FFmpeg

    # 3. Package:
    powershell -ExecutionPolicy Bypass -File scripts\package_windows.ps1
    # -> dist\cinvar-<version>-windows-x86_64.zip and .zip.sha256

The script builds with `--locked`, assembles the tree above, runs `cinvar --version` from both the
staged and the re-extracted binary, and prints the SHA-256 it wrote.

### Why it cannot be produced on this Linux host

Cross-compiling is not a matter of adding a rustup target; the toolchain is missing on this host and
installing it needs administrator rights this session does not have:

    $ which cl.exe link.exe
    (nothing)
    $ ls /usr/bin/*mingw*
    ls: cannot access '/usr/bin/*mingw*': No such file or directory
    $ cargo build --target x86_64-pc-windows-msvc     # minimal crate
    note: please ensure that Visual Studio 2017 or later, or Build Tools for Visual Studio
          were installed with the Visual C++ option
    error: could not compile (bin) due to 1 previous error
    $ cargo build --target x86_64-pc-windows-gnu
    error[E0463]: can't find crate for std   # target not installed, and linking would still
                                             # need x86_64-w64-mingw32-gcc

Beyond the linker, CINVAR compiles C (bundled SQLite through `rusqlite`), so the GNU route needs
mingw-w64 as well. A Windows artifact therefore comes from a Windows runner (or a machine with the
MSVC tools), never from this host.

## Install and verify

    Get-FileHash .\cinvar-0.2.0-windows-x86_64.zip -Algorithm SHA256
    # compare with the published .zip.sha256, then:
    Expand-Archive .\cinvar-0.2.0-windows-x86_64.zip -DestinationPath §env:LOCALAPPDATA\cinvar
    §env:LOCALAPPDATA\cinvar\cinvar-0.2.0-windows-x86_64\cinvar.exe doctor

There is no Windows installer (.msi) yet and the zip is not code-signed, so SmartScreen will ask
once. Signing and a winget manifest are follow-ups, not something this repository does today.

## Configuration fixes on Windows

CINVAR's defaults are Unix-shaped (`~/.config/cinvar`, `~/.local/share/cinvar`). On Windows those
resolve under your profile through `USERPROFILE`, which works but puts everything in
`C:\Users\<you>\.config\cinvar` and `C:\Users\<you>\.local\share\cinvar`. Set the documented
variables instead and keep them in one place, for example in your PowerShell profile:

    §env:CINVAR_CONFIG_DIR = "§env:APPDATA\cinvar"
    §env:CINVAR_DATA_DIR   = "§env:LOCALAPPDATA\cinvar"
    §env:PATH              = "§env:LOCALAPPDATA\cinvar\bin;§env:PATH"

Points that bite on Windows specifically:

* **Long paths.** Each Pro AI component gets `<data dir>\venvs\<component>\`, and PyTorch wheels nest
  deeply. If your profile path is long, keep `CINVAR_DATA_DIR` short (`C:\cinvar-data`).
* **FFmpeg must be a full build.** The "essentials" build has no `drawtext` (no libfreetype), so
  titles and burned-in captions fail. `cinvar doctor --json` reports `drawtext_filter: false`.
* **Fonts.** Windows FFmpeg builds ship without a fontconfig configuration file, and resolving a font
  family there crashes the process (access violation, exit `0xC0000005`) instead of printing an
  error. CINVAR therefore always passes an explicit `fontfile=` on Windows (`src/render/fonts.rs`),
  mapping common families to `C:\Windows\Fonts\*.ttf` through `WINDIR`.
* **No apostrophes in a font or caption path.** FFmpeg's filtergraph cannot carry an apostrophe in a
  filename, and a `fontfile=` it cannot open is silently ignored (measured: a staged font renders
  635 bytes, the same spec with an unopenable path renders 715 bytes with the fallback face). Copy
  the font to a simple path such as `C:\fonts\Inter.ttf` and point `--font` at it.
* **GPU.** `cinvar setup --pro-ai` picks the CUDA wheel channel when `nvidia-smi` answers and the
  CPU channel otherwise; force either with `CINVAR_PRO_AI_DEVICE=cuda|cpu`. The pinned `+cu128`
  stack needs a CUDA 12.x driver (525.60.13 or newer).
* **Python.** `setup --pro-ai` needs Python 3.10+ on PATH (`winget install Python.Python.3.12` and
  tick "Add to PATH"). It creates the virtualenvs itself; do not install into the system Python.

## Uninstall

    Remove-Item -Recurse -Force "$env:APPDATA\cinvar", "$env:LOCALAPPDATA\cinvar"
    Remove-Item -Recurse -Force "$env:USERPROFILE\.config\cinvar", "$env:USERPROFILE\.local\share\cinvar"

# Packaging CINVAR

Every distributed CINVAR build comes from one release binary per platform, and every artifact is
published next to its SHA-256 so a download can be verified before it is executed.

## Artifact contract

| Platform | Artifact | Checksum | Built by |
|---|---|---|---|
| Linux x86_64/arm64 | `cinvar-<version>-linux-<arch>.tar.gz` | `.tar.gz.sha256` | `scripts/package_linux.sh` |
| Linux (single file) | `cinvar-<version>-<arch>.AppImage` | `.AppImage.sha256` | `scripts/build_appimage.sh` |
| Windows x86_64/arm64 | `cinvar-<version>-windows-<arch>.zip` | `.zip.sha256` | `scripts/package_windows.ps1` (on Windows) |
| macOS | `cinvar-<version>-macos-<arch>.tar.gz` | `.tar.gz.sha256` | `scripts/package.sh` on a macOS runner |
| Homebrew | `Formula/cinvar.rb` | digests embedded per platform | `scripts/render_homebrew_formula.sh` |

Artifacts land in `dist/` (override with `DIST`), are named after the `Cargo.toml` version, and
never include `pro/`, issuer keys or model weights.

## Linux tarball (reproducible)

`bash
CARGO_TARGET_DIR=/tmp/tgt-package-linux scripts/package_linux.sh --reproducible
`

The archive is byte-identical for the same source, `Cargo.lock` and toolchain:

* `SOURCE_DATE_EPOCH` defaults to the commit time and is stamped on every file;
* `tar --sort=name --mtime=@EPOCH --owner=0 --group=0 --numeric-owner` and `gzip -9n`;
* the binary comes from a target directory outside the tree, built with `--locked`;
* `--reproducible` packs the same staging directory twice and fails if the two sha256 differ.

The script also extracts the archive and runs `cinvar --version` from it, so a package that cannot
start is never published.

## AppImage

`bash
CARGO_TARGET_DIR=/tmp/tgt-package-linux scripts/build_appimage.sh
`

Assembles an AppDir (`usr/bin/cinvar`, `cinvar.desktop`, `cinvar.svg`, `AppRun`, docs) and packs
it with `appimagetool`. The tool is downloaded once per machine into
`CINVAR_TOOLS_DIR` (default `~/.cache/cinvar-tools`) and its sha256 is printed, because the upstream
"continuous" channel is not version-pinned; set `CINVAR_APPIMAGETOOL` to use a pinned local copy.
Building needs no FUSE (`--appimage-extract-and-run`), and the smoke test extracts the finished
AppImage and runs `cinvar --version`. AppImages are **not signed**: signing needs a release key this
repository does not hold.

## Homebrew formula

`bash
scripts/render_homebrew_formula.sh --version 0.2.0 \
  --linux-x86_64 dist/cinvar-0.2.0-linux-x86_64.tar.gz \
  --macos-arm64  dist/cinvar-0.2.0-macos-arm64.tar.gz
`

Digests are computed from the artifacts themselves and cross-checked against any sibling
`.sha256`; a mismatch aborts the render. A platform without an artifact renders an explicit
`odie` ("no release artifact") instead of a download URL that would 404. The rendered formula is
validated with `ruby -c` when ruby is available, and its `test do` block runs `cinvar --version`
plus `cinvar doctor --json`.

## Windows

`scripts/package_windows.ps1` must run on Windows: CINVAR's Windows binary is MSVC-linked and is not
cross-compiled here. `docs/WINDOWS_DISTRIBUTION.md` states the exact reason and the configuration
fixes Windows users need.

## Verifying a download

`bash
sha256sum -c cinvar-0.2.0-linux-x86_64.tar.gz.sha256
scripts/install-prebuilt.sh dist/cinvar-0.2.0-linux-x86_64.tar.gz
`

`scripts/install-prebuilt.sh` refuses plain `http`, refuses to install without a checksum, installs
into `~/.local` (never sudo), then runs `cinvar doctor` and optionally `cinvar setup --pro-ai`.

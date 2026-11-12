# OpenTimelineIO

- **Export**: `cinvar otio export --output timeline.otio` / `cinvar.otio.export {output}`; writes inside the project only, `.otio` only.
- **Import**: `cinvar.otio.import {input}` accepts `.otio` files whose media references are **content-validated** and
  confined to the project, the `.otio`'s own folder, or `security.media_roots`. Symlinks, `..` escapes and unlisted absolute
  paths are refused. Files are copied into the project before use.
- Verified against the real OpenTimelineIO 0.18.1 library (`CINVAR_OTIO_PYTHON`, test-only helper).
- Limits: simple track/clip/gap/transition structures only; effects, nested stacks and non-file media references are
  not preserved. Round-tripping through another editor has not been tested.
- Security notes: [SECURITY_REVIEW_PACKAGE.md](SECURITY_REVIEW_PACKAGE.md) (the path re-check against swapping is Linux-only).

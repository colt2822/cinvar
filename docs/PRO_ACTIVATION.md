# Activating CINVAR Pro

CINVAR Pro is the CINVAR core plus a separately licensed Pro module. A signed license file switches
the Pro capabilities on. Activation is offline: no account, no network call, no telemetry.

    cinvar edition activate --file /path/to/license.json
    cinvar edition status          # shows: edition: pro

Or open the web UI (`cinvar ui`), click the edition badge, and use **Edition & license**.

To go back to Free: `cinvar edition deactivate` (your projects are unaffected; they are plain project
folders that open in either edition).

The license is a small signed JSON file kept at `~/.config/cinvar/license.json` (or under
`$CINVAR_CONFIG_DIR`). It holds an opaque order reference, not your name or email. A license that does
not verify is ignored and the app runs as Free; `cinvar edition status` says why. Perpetual means it
does not expire. Licenses carry a format version; newer fields are ignored by older builds.

## What Pro adds

| Capability | Tools / UI |
|---|---|
| Export above 2048x1080 (4K, 8K) | `render` / Export |
| Batch rendering | `cinvar.render.batch_*`, Pro panel > Batch render |
| Saved export presets | `cinvar.preset.*`, Pro panel > Export presets |
| Advanced color grading | `cinvar.color.*`, Pro panel > Advanced color |

Cloud rendering stays labeled experimental. AI auto-editing is not part of this release.

## Free download + Pro license

A Free download does not contain the Pro module. If you install a valid license into it, `edition
status` reports that the license is valid but this download lacks the Pro module. The export
ceiling is a core capability, so the license still lifts it (4K/8K export works); batch rendering,
presets and advanced color need the Pro download. Projects graded with Pro color still open and render in Free; the
grade is skipped (a warning is shown) rather than failing.

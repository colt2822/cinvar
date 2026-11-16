# CINVAR Visual QA

`cinvar.qa.render_check` / `cinvar qa <FILE>` runs these measured checks on any rendered file:

| Check | Method | This sprint |
|---|---|---|
| file_exists | size > 1 KB | existing |
| video_stream | ffprobe | existing |
| resolution | vs timeline canvas | existing |
| duration | vs timeline, ±2 % (≥0.5 s) | existing |
| audio_present | ffprobe + timeline expectation | existing |
| black_frames | blackdetect ≥0.10 s | existing |
| black_flash | isolated black frames inside the programme | existing |
| audio_levels | volumedetect mean/max (silent/clipping) | existing |
| **frozen_frames** | `freezedetect=n=0.003:d=2.0` | **new** |
| **dead_air** | `silencedetect` ≥2.5 s at −45 dB inside the programme (head/tail exempted) | **new** |

Skipped checks are reported with a reason and do not affect the score; every check is always present in the report.

## Verified live
- Crafted file with an 8 s silent stretch (10–18 s): `dead_air` FAIL at `10.01-18.02s` — exact match.
- Real autoedit renders: `frozen_frames pass`, `dead_air pass`, score 1.000.

## Correction loop
`cinvar auto-edit` renders → inspects → corrects → re-renders, bounded by `--max-revisions` (default 2). Verified: a render failing `audio_levels` was corrected (+6 dB) and re-rendered to a pass. Unmeasurable and subjective findings are reported, never auto-"fixed".

## Not yet automated
Frozen-frame correction (reselecting a different source window), caption readability/overflow checks, framing/subject-crop vision checks.

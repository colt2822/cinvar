# CINVAR Director's Room - progress

HEAD at start: 6d52d06 (verified)

## Sonnet's review of the First Light videos (own observations, frames + ffmpeg, 2026-11-10)
- Both deliverables are built from flat colour cards ("SHOT n" baked into pixels). They prove the pipeline, not editing quality.
- v4 (18 s): hard cuts between cards; lower-third "FIRST LIGHT" on shot 5 only; ends in black. Loudness -15.3 LUFS (louder than the -14 streaming target is fine, LRA 6.3).
- v2 (16 s, "revised"): the revised title is centred and collides with the baked-in "SHOT 5" text (unreadable overlap, 1 source of the revision's regression vs v4). Zoom visible only on shot 1; it is a static scale, not animated.
- v2 loudness -15.0 LUFS, peak -7.2 dBFS; cuts at 2, 8, 11 s.
- Renderer facts (src/render/graph.rs): keyframes use only the first value (`keyframe_value`), warning emitted; text has no animation (animate_in unused). Multi-track compositing is UNTESTED beyond 2 tracks (not known to be broken).
- Evidence limits: frame review was two 0.5 fps contact sheets; no audio was heard (loudness measurement only). Demo traces (trace.json) store only aggregate cost/turns, not per-call logs, so the 74-call figure cannot be broken down by tool. Action item: persist an MCP call log.
- Baseline `cargo test`: 102 lib pass; `w3_mcp_protocol::plugin_tools_are_discovered_registered_and_executed` FAILED once with ETXTBSY (Text file busy) spawning a freshly written plugin script. This is a real flake (write-then-exec race); fix in reliability pass.
- Fable advisory: PASS - advisor responded three times (frames and numbers only; no audio or motion).

## Engineering completed (commits on top of 6d52d06)
- 431a1b0 real keyframe animation (x/y/scale/opacity/rotation) via per-frame ffmpeg expressions, 2x supersampled; verified on rendered pixels incl. speed!=1 and range renders; unknown props/easings refused; text wrap + animate_in/out; `cinvar.timeline.animate` presets.
- 722deac sidechain ducking (was a recorded-but-ignored flag): -8.9 dB under speech, full recovery; two-pass loudness (-13.9 LUFS for -14 target).
- 4475c82 compositing bug found by a 4-layer pixel test: contain-fit upper-track clips blacked out layers below. Fixed (transparent padding).
- 7be4ac1 captions: SRT/VTT import, script timing (estimate), optional engine hook (verified vs STUB only; no real whisper here), background box, wrapping.
- 7bdf801 reliability: project lock, stale-save refusal, atomic render output, symlink-safe paths, ETXTBSY retry, MCP workflow guide, per-call log.
- 4bdb1d8 `cinvar connect claude`.
- Tests: 182 pass / 0 fail.

## Real Claude run A: cinematic trailer (sonnet via claude -p, MCP, project-scoped config)
- 75 turns, $0.458, 40 MCP calls, 0 tool errors (sessions/mcp-calls.jsonl). Used skills.load, contact_sheet x5, animate x2, frames x4, preview+export, qa.
- Output trailer-r19.mp4: 25.0 s 1080p24, -13.8 LUFS, peak -2.9 dBFS, black flashes at 5.458/10.458 s (engine bug, since fixed; QA black_flash check added); title fades in/out and is legible.
- Model's own critique: footage abstract, ending soft, title font thin, music bed flat (synthetic). It did not re-check cut frames after last title change (honest disclosure).
- Footage is procedural (ffmpeg lavfi): proves tool control, NOT shot-selection taste on photographic footage.

## Advisor (Fable 5.2) reviews - evidence limits stated
Fable responded via the advisor tool twice. It saw only frames/contact sheets and numbers I put in the transcript: no motion, no audio. Timecodes +/-1 s.
1. Review of First Light v4/v2 (0.5 fps sheets): metronomic pacing, v4 title weakest element, v2 title collides with baked "SHOT 5" text and QA still scored 1.000 (revision loop does not protect earlier fixes; QA has no legibility signal). Verdict: flat cards prove plumbing only.
2. Review of trailer-r19: caught black flash frames at ~5.46/10.46 s that I and Claude's own frame checks missed (confirmed with blackframe), suspected preview!=export text scaling (confirmed: preview is 640x360), pacing runs backwards (5.5/5/2/9/3.5 s), test pattern reads as broken, title named after a file, no final beat.
Sonnet decisions: fixed both defects (root cause of the flash: redundant fps filter after the animated overlay, not the eof_action the advisor guessed - bisected with ffmpeg); added QA black_flash check; scaled text/offsets by render/timeline width; added trailer pacing rules to the cinematic skill; lock trusts live pid. Not adopted: none outright; the eof_action=repeat hypothesis was wrong but harmless and kept.

## Real runs (all real claude-sonnet-5-5 through MCP; project-scoped config)
| Run | Turns | Cost | MCP calls | Errors | Output |
|---|---|---|---|---|---|
| A trailer | 75 | $0.458 | 40 | 0 | A1 (r19) 25 s, -13.8 LUFS; QA now FAILS it (black_flash) |
| A2 same project, fixed engine | - | - | - | - | A2 passes QA 1.000 (engine before/after, no model) |
| C revision (5 director's notes) | 74 | $0.543 | 41 | see logs | C: 13.0 s, shots 3/2.5/2/1.5 s, title "WHERE LIGHT BEGINS"; music fades + push-ins survived (project.json diff) |
| B vertical short | 79 | $0.512 | 46 | 1 (insert_clip conflict, recovered) | B: 1080x1920 15 s, captions from_script, ducking |
Total model spend about $1.51. Outputs in demos/directors_room/outputs/ (not committed, 72 MB). Call logs in demos/directors_room/calllogs/.
- Ducking on B measured by A/B render of the same model-authored project: music 165 Hz band 6.3 dB lower with ducking on.
- Caption timing in B is an ESTIMATE (voice-over stand-in is a pulse tone, not speech).

# CINVAR Director's Room - final report

Sonnet 5.5 executed; Fable 5.2 advised through the advisor tool. Everything below was verified by running it; limits are stated.

## Report

    SONNET_EXECUTION=PASS
    FABLE_ADVISORY=PASS            (responded three times; saw frames/numbers only - no audio, no motion)
    CINVAR_COMMIT=1041602 (engine; later commits are docs only)
    KEYFRAME_ANIMATION=PASS        (x, y, scale, opacity, rotation measured on rendered pixels; speed!=1 and range renders; text supports x/y/opacity + fade/slide, not scale/rotation - refused loudly)
    MULTITRACK_RENDERING=PASS      (4 stacked layers z-order/timing/animated PiP on pixels; found+fixed a real blackout bug; 3+ tracks tested, rotated sources untested)
    AUDIO_MIXING=PARTIAL           (ducking real and measured; two-pass loudness lands -13.9/-14.0 LUFS; no per-clip dynamics/EQ/dialogue clean-up; audio never listened to by a human or model)
    CAPTIONS=PARTIAL               (SRT/VTT import, script timing [estimate], wrapping, background box verified; transcription engine hook verified against a STUB only - no real whisper on this machine)
    CLAUDE_MCP_EXPERIENCE=PARTIAL  (4 real headless runs via `claude -p` over MCP, 169 logged calls, 5 `ok:false` [1 render.export, 2 render.status, 1 insert_clip conflict, 1 trim], all recovered; project-scoped .mcp.json shows 'Pending approval' - the approval flow, `--apply` and an interactive session were NOT exercised)
    CREATIVE_REVIEW=PARTIAL        (real model edits + advisor critique, but footage is procedural abstract motion; no photographic footage or real speech reviewed)
    CONVERSATIONAL_EDITING=PASS    (5 director's notes applied; earlier fades/push-ins preserved, verified by project diff)
    PROJECT_RELIABILITY=PARTIAL    (lock, stale-save refusal, atomic render, symlink-safe paths, ETXTBSY, malformed calls; plugin permissions still unenforced; no render-job recovery after process death; Windows/macOS unverified)
    TESTS_PASSED=186
    TESTS_FAILED=0
    RELEASE_STATUS=ALPHA

## Original-video critique (First Light)
Both "real" videos were flat colour cards. The revision (v2) was worse than v4: the enlarged title collided with the picture's baked-in text and QA still scored 1.000. Keyframes were silently static, ducking was a flag nothing read, upper-track contain clips blacked out layers beneath them.

## Fable's recommendations -> Sonnet's decisions
Adopted: fix black flashes + add QA check; scale preview text; trailer pacing/verification rules in skills; revision as the real test; lock should trust live pid. Corrected: advisor guessed eof_action; bisecting showed a redundant fps filter. Not done: photographic footage review; hearing audio.

## Engineering (see git log)
Keyframes/animation presets, text wrap+motion, ducking, two-pass loudness, transparent letterbox, captions suite, project lock, atomic renders, MCP guide+call log, `cinvar connect claude`, QA black_flash, skills, 40+ new tests.

## Real before/after outputs (demos/directors_room/outputs/, not committed)
- A1 -> A2: same project r19; A1 has black frames at 5.458/10.458 s (QA 0.875), A2 rendered by the fixed engine passes (1.000).
- A1 -> C: 25 s abstract trailer -> 13 s revised trailer after 5 director's notes (pacing builds, test pattern gone, new title).
- B: vertical short with captions and ducked music.

## B2 (vertical short, 6 notes, run inside the project)
Applied: captions as 5 whole phrases, shots 5.5/4.5/3/2 s, gold CTA without box, disc x-keyframes; ducking and music fade-out survived. $1.73, 61 turns. QA 0.875: black_flash at 10.000 s. Measured luma goes 78 -> 31 across that hard cut: it is the dark opening of the ember clip, not a missing frame; the detector (pix threshold 0.10) is conservative, so this is a creative/detector near-miss, not an engine fault. Captions sit at ~67% frame height (the model's own report flagged this). I viewed 6 sampled frames only. Files: prompts-b2.txt, traces/b2-revision.json, traces/short-before-revision.json, calllogs/.

## Audio measurements (unreconciled)
Stem level on project B: voice -16.1 LUFS, music -22.7 undocked, -38.7 ducked (voice-music ~22.6 dB, deeper than the skill's 12-18 dB target; ratio 4/6 changed it by only 2.6/1.3 dB). An earlier A/B at the 165 Hz band showed only 6.3 dB but is contaminated by voice leakage and loudnorm. Treat both as bounds. Nothing was listened to.

## Cost
About $3.2 across the four logged runs (A $0.46, C $0.54, B $0.51, B2 $1.73), each capped at $2.

## Interruption test
SIGKILL of `cinvar mcp serve` mid-render: no final file, no partial, no stale lock; project reopened fine; the render job was not recoverable (re-export needed).

## Remaining limitations
Dissolves do not crossfade audio; render jobs are not resumable; the revised trailer C title card holds ~3.7 s on black; C's fade-from-black takes 1 s and its shortest shot is mostly fade-out; B has been revised but not independently re-reviewed by the advisor. Photographic footage and real speech never reviewed; transcription unverified with a real engine; caption timing from scripts is estimated; no loudness-aware music ducking depth control; render jobs lost if the server dies mid-render (partial file is cleaned, job not resumed); plugin network/write permissions unenforced; macOS/Windows unverified; the title card in the revised trailer holds ~3.7 s on black (30% of runtime) - a creative miss not corrected.

## Exact commit
Last engine commit: 1041602 (fmt/clippy). Run A ran on the PRE-fix engine (black-flash bug present); A2, C, B and B2 ran on the fixed engine. 186 tests pass, `verify_all.sh` 0 failures. CI's `clippy -D warnings` is not clean on this newer local toolchain (baseline 6d52d06 already had 42 errors; ~70 locations now, mostly pre-existing).

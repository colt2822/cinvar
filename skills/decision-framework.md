---
name: decision-framework
title: Editing decision framework
summary: How to choose style, pacing, transitions, titles and audio deliberately, and when to use nothing at all.
tags: [framework, planning, pacing, restraint]
version: 1
---

## Objective

Turn a vague request ("make it cinematic", "make it better") into a small number of
deliberate decisions before touching a single clip. Restraint is the default: every effect,
transition and title has to earn its place, and the strongest edit is usually the one with
the fewest moves.

Use this skill together with the style skill (cinematic, youtube, short-form, podcast,
gaming, documentary, commercial, motion-graphics, video-restoration). This one decides how
much to do; the style skill decides what to do.

## Creative principles

1. Decide the audience and the delivery surface first: a feed-scrolling phone viewer and a
   seated cinema viewer need opposite pacing. State the assumption out loud in your plan.
2. Duration is a budget, not a target. A 30-second request means 30 seconds, not 45 with a
   good excuse. Count the shots you need before you cut: roughly duration / average shot
   length.
3. Every cut needs a reason: new information, new location, new speaker, escalation, or a
   beat change. If you cannot name the reason, do not cut.
4. One idea per section. A trailer has a hook, an escalation and a button; a tutorial has a
   promise, steps and a result. Do not mix three structures in one clip.
5. Audio continuity matters more than visual polish. A cut that lands on a downbeat feels
   intentional; the same cut 200 ms early feels broken.
6. Titles exist to orient, not to decorate. One title card, plus captions where there is
   speech, is usually the entire graphics budget.
7. Colour and effects serve contrast between sections. If every shot has the same look, the
   look stops communicating.
8. Protect the subject: never cut in the middle of a word, a gesture or a movement that the
   viewer is tracking.

## Planning sequence

1. `
   cinvar.system.capabilities {}
   ` - know what the machine can actually do before promising hardware encoding, Blender or
   analysis.
2. `
   cinvar.media.import_folder {"path": "<folder>"}
   ` then `
   cinvar.project.status {}
   ` - inventory every asset with its real duration, resolution and audio presence.
3. `
   cinvar.media.scenes {"asset_id": "<id>", "threshold": 0.35}
   ` for each asset you intend to use. This gives you the shots that actually exist, so you
   choose shots instead of guessing at timestamps.
4. `
   cinvar.audio.analyze {"asset_id": "<id>"}
   ` on assets with speech or music. Loudness decides gain, not taste.
5. Write the plan in the conversation before executing it: target duration, section
   breakdown with per-section seconds, chosen shots with in/out points, the one transition
   family you will use, the title, the audio treatment. Then execute that plan.
6. `
   cinvar.timeline.create {"width": 1920, "height": 1080, "fps": "30"}
   ` (or 1080x1920 for vertical), then `
   cinvar.timeline.append_clip {"asset_id": "<id>"}
   ` and `
   cinvar.timeline.trim {"clip_id": "<id>", "source_in": "0.5", "source_out": "3.2"}
   `.
7. `
   cinvar.timeline.reorder {"track_id": "<id>", "clip_ids": ["c1","c2","c3"]}
   ` when the sequence changes as a whole; `
   cinvar.timeline.move {"clip_id": "<id>", "at": "4.0"}
   ` for one clip.
8. `
   cinvar.timeline.text {"text": "TITLE", "at": "0", "duration": "2.5", "size": 96, "position": "center"}
   ` at most once, then `
   cinvar.timeline.clip_audio {"clip_id": "<id>", "gain_db": -3}
   ` or `
   cinvar.timeline.clip_audio {"clip_id": "<id>", "normalize": true}
   `.
9. `
   cinvar.render.preview {}
   ` then `
   cinvar.render.status {"job_id": "<id>", "wait_seconds": 60}
   `, then look at it: `
   cinvar.media.frames {"path": "renders/<preview>.mp4", "times": ["0","2","5"]}
   ` and open the returned images.
10. Fix only what the frames prove is wrong, then `
    cinvar.render.export {"width": 1920, "height": 1080}
    `, `
    cinvar.render.status {"job_id": "<id>", "wait_seconds": 120}
    ` and `
    cinvar.qa.render_check {"path": "renders/<final>.mp4"}
    `.

## Timing guidance

| Format | Average shot | Hook | Transitions | Titles |
|---|---|---|---|---|
| Cinematic trailer | 1.5-3.0 s | first 2 s | 2-4 total, fades between sections | 1 card |
| YouTube long form | 4-8 s | first 15 s | hard cuts; fades only at chapters | intro + lower third |
| Short form vertical | 0.8-2.0 s | first 1.0-1.5 s | hard cuts; one whip or none | captions, one hook card |
| Documentary | 6-12 s | first 30 s | hard cuts, dissolves for time jumps | lower thirds |
| Commercial spot | 1.0-2.5 s | first 1 s | 1-2, on the product beat | logo end card |
| Podcast clip | 3-6 s | first 3 s | hard cuts, no fades | speaker lower third |
| Gaming highlight | 0.5-1.5 s | first 1 s | hard cuts, zoom punches only on events | none |

Pacing rules that survive contact with real footage: cut on action, cut on audio beats, and
never cut faster than the viewer can recognise the subject (roughly 0.6 s for a new scene).
Slow sections should be genuinely slower, not just fewer cuts.

## Revising an existing edit

- Read the timeline first (cinvar.project.status) and list what must survive: music fades, motion presets, titles, transitions. Change only what the note asks for.
- Name the rollback revision to the user before you start (cinvar.project.history).
- After the change, compare: durations, which clips moved, which keyframes/fades are still there. A revision that quietly drops an earlier fix is a regression even if the new note is satisfied.
- Re-check the frames around everything you touched, including titles, at the new render size.

## Evaluation checklist

- `
  cinvar.qa.project_check {}
  ` returns no missing assets, overlaps or zero-length clips.
- Measured duration is within 5% of the requested duration.
- Every clip in `
  cinvar.project.status {}
  ` has a source range you can justify.
- The frames you extracted at the start, middle and end show the title inside the safe area
  and the subject not cropped by a reframe.
- `
  cinvar.qa.render_check {"path": "<final>"}
  ` passes, with audio present and no clipping.
- You can state, in one sentence per section, why that section exists.

## Failure conditions

Stop and ask, or report the limitation plainly, when:

- the footage does not contain enough distinct shots to fill the requested duration at the
  chosen pace (say so instead of padding with freeze frames);
- the requested style contradicts the material (a 30-second cinematic trailer from one
  static 4-second clip is not achievable - offer a shorter cut or a different format);
- required audio is missing, clipped beyond repair, or unusable;
- a requested effect does not exist in CINVAR (report the supported list rather than
  faking it);
- you cannot inspect the result because no frames could be extracted.

## Worked example

Request: "Cinematic trailer from these clips, 30 seconds, 1080p."

Plan: hook 0-3 s (strongest frame, no text), escalation 3-15 s (four 2-3 s shots, hard cuts,
one fade at 15 s), build 15-25 s (faster 1.5 s shots, one speed ramp), button 25-30 s (title
"PROJECT NAME" over the widest shot, fade to black). Ten shots, two fades, one title, music
normalised to -14 LUFS, dialogue not buried.

Execution: import, scenes on each asset, pick ten in/out points, append in order, trim,
one `
cinvar.timeline.transition {"clip_id": "<id>", "side": "in", "kind": "fade", "duration": "0.6"}
` at the section boundary, `
cinvar.timeline.text {"text": "PROJECT NAME", "at": "25", "duration": "5", "size": 110, "position": "center"}
`, per-clip gain so the loudest source sits at -3 dB, preview, inspect three frames, export,
QA. Ten shots, two transitions, one title. Nothing else.

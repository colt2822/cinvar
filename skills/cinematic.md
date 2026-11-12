---
name: cinematic
title: Cinematic Narrative Edit
summary: Build a restrained narrative sequence from long footage: motivated cuts, held frames, one point of view, disciplined sound.
tags: [cinematic, narrative, long-form, color]
---

## Objective

Turn rushes into a sequence that reads as a film. One point of view, cuts motivated by action or gaze, visible restraint, and an audio bed that carries emotion without announcing itself. Respect the shot language the material already has. Never impose a rhythm the footage cannot support.

## Creative principles

- Establish geography before you fragment it. The audience must know where the people and objects are before you cut close.
- Cut on action, on a look, or on the completion of a gesture. Never cut on a coincidence of timecode.
- Hold when the frame is doing work. A six-second hold on a face outlasts three two-second cutaways.
- Protect screen direction. A subject walking left must keep leaving frame left unless you show the turn.
- Keep one idea per cut. If a shot changes location, subject and mood at once, it is a montage, not a cut.
- Use sound to bridge: carry an outgoing ambience under the incoming picture for 12-20 frames so the cut is felt before it is seen.
- Grade toward a single look. Set contrast and saturation once, then vary exposure only where the story needs it.
- Do not add a transition to hide a bad cut. Fix the cut, or leave the air.

## Planning sequence

1. Read the project before touching it.
    cinvar.project.status {}

2. Confirm the toolchain so you know a render will actually run.
    cinvar.system.capabilities {}

3. Import the scene folder in one pass, then read what came in.
    cinvar.media.import_folder {"path": "/footage/scene12", "recursive": true}

4. Probe the two or three takes you intend to use. Note duration, resolution, frame rate and whether audio exists.
    cinvar.media.inspect {"path": "/footage/scene12/A003_C012.mp4"}

5. Build proxies for anything above 1080p so previews stay fast.
    cinvar.media.proxy {"asset_id": "as_7f3c21d8", "height": 540}

6. Look at the footage before deciding cut points. A contact sheet per take, then exact frames around the candidate.
    cinvar.media.contact_sheet {"asset_id": "as_7f3c21d8", "count": 12, "columns": 4}
    cinvar.media.frames {"asset_id": "as_7f3c21d8", "times": ["14.5", "18.0", "21.4"], "width": 960}

7. Detect shot boundaries so you cut at real edges, not guesses.
    cinvar.media.scenes {"asset_id": "as_7f3c21d8", "threshold": 0.32}

8. Create the canvas at the delivery frame rate.
    cinvar.timeline.create {"name": "Scene 12", "width": 1920, "height": 1080, "fps": "24"}

9. Place shots with explicit source in and out points. Never rely on the full asset duration.
    cinvar.timeline.insert_clip {"asset_id": "as_7f3c21d8", "at": "00:00:00.000", "source_in": "00:00:14.500", "source_out": "00:00:21.000"}

10. Trim the head of the incoming shot to the frame the performance starts.
    cinvar.timeline.trim {"clip_id": "cl_2be71a", "source_in": "00:00:16.250"}

11. Balance sound per clip, then check the whole mix.
    cinvar.audio.analyze {"asset_id": "as_7f3c21d8"}
    cinvar.timeline.clip_audio {"clip_id": "cl_2be71a", "gain_db": -3.0, "fade_in": "0.5", "fade_out": "0.75", "normalize": true}

12. Add a title only if the film needs one, set low and left, and give it air.
    cinvar.timeline.text {"text": "NORTH COAST", "at": "4.0", "duration": "3.0", "size": 54, "color": "#f2efe9", "position": "lower_third", "outline": false}

13. Preview, watch it end to end, then export.
    cinvar.render.preview {}
    cinvar.render.status {"job_id": "job_19c4"}

## Timing guidance

- Dialogue coverage: 4-9 seconds per shot, cut when the line lands, not when the sentence ends.
- Inserts and reactions: 1.2-2.5 seconds. Anything under 1 second reads as a flash and needs a reason.
- Establishing shots: 3-5 seconds, or hold into the scene if the frame carries atmosphere.
- Fades and dissolves: 0.5-1.5 seconds. Longer than 2 seconds reads as a chapter break.
- Keep the first cut inside 12 seconds of picture start; a cold open longer than that loses the room.
- Final mix: target -16 LUFS integrated with true peak under -1.5 dBTP.

## Trailers and teasers (observed mistakes, fix them first)

- Shots get SHORTER toward the title, not longer. A first-run edit ran 5.5 / 5 / 2 / 9 / 3.5 s and slowed into its climax. Plan the shot-length list before cutting, and make it fall: for example 3.0, 2.5, 2.0, 1.5, 1.0, then one held beat for the title.
- The first shot is 3 seconds or less and is the strongest image, not a fade from black. Open on picture; if you fade up, keep it under 12 frames.
- Never play a source clip in full by default. Choose the span where the picture still changes and cut the rest. If the last third of a shot is visually static, it is dead air.
- If a shot clashes (a test pattern, a different look, a different aspect) cut it. Do not keep it and call it a "hit"; naming a flaw is not fixing it.
- The title is the last beat and holds alone on a calm frame for about a second. Name it from the story or the user's words, never from a file name. Prefer a heavier weight and a size readable at thumbnail scale; give the user the placeholder-name caveat in your answer.
- A score that does not change cannot build. Cut on its pulse or bring it up under the title; a flat bed means the picture must carry the escalation.
- Use cinvar.timeline.animate push_in on held shots so nothing is a still frame, but vary direction (push_in then pan_left) rather than repeating one move.

## Verify before you say done

- Extract frames at every cut +/- 2 frames and at the title's start, middle and end. Open them.
- Run cinvar.qa.render_check; it includes an isolated-black-frame check. A pass is necessary, not sufficient: it cannot judge taste.
- After ANY edit to a clip or title, re-check the frames around it. Do not report a frame check you did not repeat after the last change.
- Say plainly what you could not verify (you cannot listen to audio; you can only measure levels).

## Evaluation checklist

- Every cut lands on action, gaze or a completed gesture.
- Geography holds across the sequence; no 180-degree break without a neutral shot.
- No shot enters or exits on a dead frame or a blink.
- On-screen text is legible at 100 percent scale and has an outline only if it sits over busy detail.
- The mix does not pump when a clip with gain change follows a normalised one.
- cinvar.qa.project_check reports no overlaps, zero-length clips or missing assets.
- A preview was watched in full before export.

## Failure conditions

- Cutting to a template rhythm instead of the material. The edit will read as stock.
- Importing the folder and trusting file names for content. Always look at frames first.
- Applying crossfades to hide jumps in performance or exposure.
- Raising gain per clip until the mix clips; check cinvar.audio.clipping when in doubt.
- Exporting without a render check, then reporting a duration the file does not have.
- Claiming the sequence is finished when only the rough cut exists.

## Worked example

A 90-second opening from three takes. Import the folder, probe the wide, build a 540p proxy, and pull a 12-frame contact sheet. Shot boundaries put the usable wide between 14.5 and 24.0 seconds. Place the wide at 0.000 with source_in 14.500 and source_out 21.000, then the medium at 6.500 with source_in 02.100 and source_out 09.400. Cut the close-up of the hands as an insert at 9.400 for 1.6 seconds. Trim the wide's tail so the action completes two frames before the cut. Lower the wide 2 dB, normalise the medium, and run a 0.75-second fade out on the final clip. Preview, check loudness, export at CRF 18 with h264, then verify with cinvar.qa.render_check at the expected 27.6 seconds.

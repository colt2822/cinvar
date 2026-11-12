---
name: youtube
title: YouTube Long-Form Edit
summary: Structure a 8-25 minute YouTube video: hook first, retention beats every 40-60 seconds, clean captions and a legible thumbnail frame.
tags: [youtube, long-form, tutorials, retention]
---

## Objective

Deliver a video that keeps a browsing viewer past the first 30 seconds and rewards the watch to the end. The structure does the work: a concrete promise in the first 15 seconds, a visible payoff every 40-60 seconds, and an ending that closes the loop it opened. The edit must survive being watched at 1.5x with the sound off.

## Creative principles

- Open on the result, not the setup. Show the finished thing, then explain how it was made.
- One idea per section. If a section needs a second idea, split the section.
- Every 40-60 seconds something must change: framing, graphic, location, or the subject being discussed.
- Write for the ear. Short sentences, concrete nouns, no throat-clearing.
- Captions are part of the picture: burn them, place them bottom-centre, and keep them out of the lower third where UI overlays live.
- Keep the loudness consistent across segments recorded at different times; audiences hear a 2 dB jump immediately.
- Leave a visible cut rhythm. Jump cuts are acceptable when the jump is under 0.4 seconds of removed air.
- End on the next action, not on a summary of what was just watched.

## Planning sequence

1. Read project state and any existing timelines.
    cinvar.project.status {}

2. Import the recorded segments.
    cinvar.media.import_folder {"path": "/footage/tutorial_ep04", "recursive": false}

3. Probe each segment for resolution and audio before editing, so mismatched takes surface early.
    cinvar.media.inspect {"path": "/footage/tutorial_ep04/seg01.mp4"}

4. Remove dead air from talking-head takes.
    cinvar.audio.silence_plan {"asset_id": "as_44c1e0aa", "noise_db": -34, "min_secs": 0.6, "keep_secs": 0.18}

5. Map the sections before assembly: hook, setup, section A, section B, payoff, close.
    cinvar.timeline.create {"name": "EP04 Long Cut", "width": 1920, "height": 1080, "fps": "30"}

6. Assemble in order with the hook first, even if it was recorded last.
    cinvar.timeline.append_clip {"asset_id": "as_44c1e0aa"}
    cinvar.timeline.insert_clip {"asset_id": "as_90f2b17c", "at": "00:00:12.000", "source_in": "00:03:22.400", "source_out": "00:04:05.000"}

7. Split a clip wherever you need to cut mid-section rather than re-placing the whole asset.
    cinvar.timeline.split {"clip_id": "cl_71aa03", "at": "00:02:14.500"}

8. Close gaps left by removals so the cut does not fall into silence.
    cinvar.timeline.remove {"clip_id": "cl_71aa03", "ripple": true}

9. Add graphics and lower thirds through the text tool, not as separate assets.
    cinvar.timeline.text {"text": "Step 2: colour match", "at": "00:04:10.000", "duration": "4.0", "size": 48, "color": "white", "position": "lower_third", "template": "lower_third"}

10. Attach captions as one cue list, then check the burn.
    cinvar.timeline.captions {"cues": [{"start": "0.4", "end": "3.1", "text": "This is the part that usually breaks."}, {"start": "3.2", "end": "6.0", "text": "Here is the fix, start to finish."}], "language": "en", "size": 52, "position": "bottom"}

11. Level segments recorded at different times.
    cinvar.audio.analyze {"asset_id": "as_44c1e0aa"}
    cinvar.timeline.clip_audio {"clip_id": "cl_0d3e99", "gain_db": -2.5, "normalize": true}

12. Preview the full running time once before export; a 14-minute video hides its flaws in the second half.
    cinvar.render.preview {}
    cinvar.render.status {"job_id": "job_5b71"}

## Timing guidance

- Hook: 0.0-3.0 seconds for the result shot; promise spoken by 15 seconds.
- Section length: 90-180 seconds. A section longer than three minutes needs an internal visual change.
- Cut cadence inside talking head: one change every 20-40 seconds minimum, jump cuts every 5-12 seconds.
- Chapter cards: 1.5-2.5 seconds, full frame, no animation longer than 0.4 seconds.
- Captions: 2-4 seconds per cue, maximum 42 characters per line, two lines maximum.
- End screen: last 20 seconds clear of text; put the call to action in the previous 20.

## Evaluation checklist

- The first 15 seconds name the payoff.
- No section runs past three minutes without a visual change.
- No dead air over 0.5 seconds in a talking-head section.
- Captions match the spoken words, stay inside safe margins, and never collide with the lower third.
- Loudness measured per segment, within 1 dB across the whole timeline.
- Thumbnail candidate frame exists and is legible at 320 pixels wide.
- cinvar.qa.render_check confirms duration within 0.5 seconds of the timeline.

## Failure conditions

- A cold open longer than 20 seconds before the promise.
- Leaving imported segments unprobed, then discovering a 720p take inside a 1080p timeline.
- Burning captions with auto-generated text that was never read.
- Cutting to a structure copied from another channel instead of the material.
- Exporting before the preview, then reporting a runtime the file does not have.
- Describing retention as improved without any measurement.

## Worked example

A 12-minute tutorial. Import four segments, probe each, run silence_plan on the two talking-head takes and remove 41 seconds of air. Create the timeline at 1080p30. Place the finished result shot first for 6 seconds, then the spoken hook. Split section A at 02:14.500 to insert a screen recording asset between two takes, remove the leftover gap with ripple. Add five lower thirds at the start of each step. Attach 78 caption cues covering the spoken track, position bottom, size 52. Normalise the two older segments by -2.5 dB. Preview, verify loudness at -14 LUFS integrated, export CRF 19 h264 with burn_captions true at 14:02, then run cinvar.qa.render_check with expected_duration 842.0.

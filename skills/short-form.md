---
name: short-form
title: Vertical Short-Form Edit
summary: Cut 15-60 second vertical video that earns attention in the first second: one hook, one idea, burned captions, a beat every 2-4 seconds.
tags: [short-form, vertical, social, captions]
---

## Objective

Produce a vertical clip that a viewer understands without prior context and without sound. One idea, one hook, one payoff. Everything that does not serve the single idea is removed, including good footage.

## Creative principles

- The first frame is the thumbnail. Start on the most interesting image you have, not on a logo or a title card.
- One clip, one claim. If you can sum it up in two sentences, the clip is one sentence too long.
- Replace every second of set-up with a second of proof.
- Cut on the beat if music is present, but never let the beat cut a word in half.
- Captions are mandatory. Place them in the middle third of the frame, away from platform UI at the bottom and the top.
- Vertical framing means the subject fills the height. Reframe rather than letterbox.
- Silence is a tool; a 0.3 second pause before the payoff is stronger than a whoosh effect.
- End on the action, not on a sign-off. The last frame should still be moving.

## Planning sequence

1. Check what is already in the project before importing anything new.
    cinvar.project.status {}

2. Import the source footage.
    cinvar.media.import_folder {"path": "/footage/reels_mar", "recursive": false}

3. Probe candidates and discard anything under 720x1280 for a vertical deliverable.
    cinvar.media.inspect {"path": "/footage/reels_mar/take03.mp4"}

4. Look at frames either side of the intended hook before committing to it.
    cinvar.media.frames {"asset_id": "as_2c91aa04", "times": ["0.2", "0.8", "1.4", "2.0"], "width": 540}

5. Remove silence from any spoken take so the hook lands on the first syllable.
    cinvar.media.silence {"asset_id": "as_2c91aa04", "noise_db": -32, "min_secs": 0.4}

6. Create a 1080x1920 canvas at 30 fps.
    cinvar.timeline.create {"name": "Vertical 01", "width": 1080, "height": 1920, "fps": "30"}

7. Open on the strongest half second, then the spoken hook.
    cinvar.timeline.insert_clip {"asset_id": "as_2c91aa04", "at": "00:00:00.000", "source_in": "00:00:06.200", "source_out": "00:00:06.900"}
    cinvar.timeline.append_clip {"asset_id": "as_2c91aa04"}

8. Split away every hesitation, false start and repeated sentence.
    cinvar.timeline.split {"clip_id": "cl_5f10b2", "at": "00:00:08.400"}
    cinvar.timeline.remove {"clip_id": "cl_5f10b2", "ripple": true}

9. Re-time a beat when a moment deserves emphasis, keeping pitch natural: 0.85 is a safe slow-down.
    cinvar.timeline.speed {"clip_id": "cl_91ab77", "speed": 0.85}

10. Add a freeze frame only where the frame itself is the payoff.
    cinvar.timeline.freeze {"clip_id": "cl_91ab77", "at": "00:00:11.500"}

11. Caption the whole spoken track. Two words per line maximum.
    cinvar.timeline.captions {"cues": [{"start": "0.0", "end": "1.2", "text": "Three seconds"}, {"start": "1.2", "end": "2.6", "text": "is all you get."}], "language": "en", "size": 64, "position": "center"}

12. Set the music bed below the voice, not on top of it.
    cinvar.timeline.clip_audio {"clip_id": "cl_music01", "gain_db": -18.0, "fade_in": "0.4", "fade_out": "0.8"}

13. Preview and check the first three seconds on a phone-sized frame.
    cinvar.render.preview {"range_start": "0", "range_end": "3.0"}
    cinvar.render.status {"job_id": "job_77a1"}

## Timing guidance

- Hook: 0.0-1.2 seconds. Show the result, the mistake, or the question.
- Payoff beat: every 2-4 seconds; nothing static for longer than 4 seconds.
- Total length: 18-45 seconds for a single claim, up to 60 only when steps are numbered.
- Caption cue: 0.8-2.0 seconds, maximum 12 characters per line on a vertical frame.
- Music fade under the final spoken word: 0.5-0.8 seconds.
- End card: 0.6 seconds maximum, and only if the platform requires it.

## Vertical reframing and captions with the current tools

- Create the timeline at 1080x1920 (9:16) first. cinvar.timeline.transform with fit "cover" fills the frame; shift x to choose which part of a 16:9 source stays in frame (positive x moves the picture right, so to keep the left of the shot use positive x).
- Animate the reframe, not just the cut: cinvar.timeline.animate pan_left / pan_right / push_in gives a slow move that keeps a wide source alive in a tall frame.
- Captions: cinvar.captions.import for an .srt/.vtt, cinvar.captions.transcribe if cinvar.system.capabilities reports an engine, otherwise cinvar.captions.from_script with the spoken text and the start/end of the speech. from_script is an ESTIMATE; use cinvar.media.silence to find where speech starts and stops, then check frames at the start, middle and end of the captions.
- Style for a phone: size 64-80, uppercase optional, a background box (black@0.55) over busy footage, position center or lower_third (stay out of the bottom 20 percent). Long lines wrap automatically inside the frame.
- Duck the music: cinvar.timeline.clip_audio {"clip_id": "<music>", "duck_under_speech": true}. It drops while any non-ducked audio plays and recovers after.

## Evaluation checklist

- The hook is legible in the first frame with sound off.
- The clip states one idea and proves it once.
- No dead air, no repeated take, no half sentence.
- Captions are inside the middle third and do not overlap platform UI zones.
- Voice sits 12-18 dB above the music bed.
- The last frame is intentional, not a trailing clip tail.
- cinvar.qa.project_check reports no overlaps or zero-length clips in the timeline.

## Failure conditions

- Starting on a title card, logo or black frame.
- Keeping two hooks because both were good; the clip then has no hook.
- Vertical letterboxing a horizontal source instead of reframing.
- Auto-captioning without reading the output.
- Hitting the loop point mid-sentence so the platform rewatch reads as a glitch.
- Claiming a retention result that was never measured.

## Worked example

A 27-second clip. Import the take, probe it at 1080x1920, pull four frames, then trim the first 6.2 seconds of setup. Create the vertical timeline, place a 0.7 second hook from 06.200-06.900, append the body, and ripple-remove three false starts totalling 4.8 seconds. Slow the final demonstration to 0.85, freeze one frame at 00:00:11.500 for 1.0 second, caption 14 cues at size 64 centred, drop the music bed to -18 dB with a 0.8 second fade out, and export at CRF 20 h264 mp4 with burn_captions true.

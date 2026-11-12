---
name: commercial
title: Commercial Spot Edit
summary: Cut a 15 or 30 second spot that lands one product promise: beauty shots first, one proof, one call to action, cut to a fixed time.
tags: [commercial, advertising, brand, short]
---

## Objective

Deliver a spot of an exact, contracted length that communicates one product promise, one proof and one action. Every frame must justify its duration at the moment the spot airs. Nothing in the edit may imply a claim the product does not make.

## Creative principles

- Write the last frame first: the brand and the call to action are fixed, then the proof, then the hook.
- One promise per spot. Two promises make the viewer remember neither.
- Beauty shots are the product: light, texture, motion. Do not waste them as background.
- The proof must be visible, not spoken. Show the result, then say it.
- Cut to a rhythmic grid so the spot survives a 15 second cutdown without re-editing.
- Legibility beats style for every super and end card: minimum 3 seconds of stillness for a URL.
- Products are never altered in a way the packaging would contradict; colour accuracy is a compliance issue, not a taste issue.
- No endorsement, award or statistic appears unless the client supplied the substantiation.

## Planning sequence

1. Read the project and confirm delivery requirements before importing.
    cinvar.project.status {}

2. Import the shooting day, separating product, talent and location folders.
    cinvar.media.import_folder {"path": "/footage/spot_anvil/product", "recursive": false}
    cinvar.media.import_folder {"path": "/footage/spot_anvil/lifestyle", "recursive": false}

3. Probe the product shots first; they set the required resolution for the whole spot.
    cinvar.media.inspect {"path": "/footage/spot_anvil/product/pack_hero.mov"}

4. Confirm the product colour before you grade. Pull exact frames of the pack at neutral exposure.
    cinvar.media.frames {"asset_id": "as_pack_hero", "times": ["00:00:02.000", "00:00:05.400"], "width": 1920}

5. Build the 30 second timeline at the mandated frame rate and duration.
    cinvar.timeline.create {"name": "Anvil 30s", "width": 1920, "height": 1080, "fps": "25"}

6. Place the hero product move across the first four seconds; hold the pack in the middle third of frame.
    cinvar.timeline.insert_clip {"asset_id": "as_pack_hero", "at": "00:00:00.000", "source_in": "00:00:01.200", "source_out": "00:00:05.200"}

7. Place the proof shot; cut on the action that demonstrates the product working.
    cinvar.timeline.insert_clip {"asset_id": "as_life_04", "at": "00:00:08.000", "source_in": "00:00:12.000", "source_out": "00:00:14.200"}

8. Trim every shot to the exact grid so a 15 second cutdown is a subtraction, not a rebuild.
    cinvar.timeline.trim {"clip_id": "cl_hero1", "source_out": "00:00:04.000"}

9. Add the supers as text, never as baked-in graphic files, so they stay editable for localisation.
    cinvar.timeline.text {"text": "One turn. Zero tools.", "at": "00:00:09.000", "duration": "2.4", "size": 62, "color": "#ffffff", "position": "center", "outline": true}

10. Build the end card: pack lockup, brand, call to action, held still.
    cinvar.timeline.text {"text": "anvil.co/start", "at": "00:00:26.000", "duration": "3.5", "size": 44, "color": "#ffffff", "position": "bottom"}

11. Set the music to the cut grid and duck it under the voice.
    cinvar.timeline.clip_audio {"clip_id": "cl_vo", "gain_db": 0.0, "normalize": true}
    cinvar.timeline.clip_audio {"clip_id": "cl_music", "gain_db": -14.0, "fade_in": "0.2", "fade_out": "1.0"}

12. Export a preview at the contracted duration and time it exactly.
    cinvar.render.preview {}
    cinvar.render.status {"job_id": "job_9e01"}

13. Export the delivery master and validate it against the contract.
    cinvar.render.export {"output": "/renders/anvil-30s-master.mp4", "crf": 16, "codec": "h264", "format": "mp4", "burn_captions": false}

## Timing guidance

- 30 second spot: hook 0-4, promise 4-10, proof 10-20, benefit 20-26, end card 26-30.
- 15 second spot: hook 0-2, proof 2-8, benefit 8-11, end card 11-15.
- Shot length: 0.8-2.5 seconds for beauty, 1.5-3 seconds for the proof, 2-4 for the benefit.
- End card stillness: minimum 3 seconds for a URL, 2 seconds for a logo alone.
- Supers: on screen 1.5-2.5 seconds, minimum 24 frames of stillness to be read.
- Audio: -24 LUFS integrated for broadcast delivery unless the brief specifies otherwise.

## Evaluation checklist

- Duration matches the contract exactly, measured on the exported file.
- One promise, one proof, one action, in that order.
- Product colour matches the approved frames; no clipped highlights in the pack.
- Every super is legible at 720p and lasts at least 1.5 seconds.
- End card is still and complete for its full duration.
- No claim, statistic or endorsement appears without client substantiation.
- cinvar.qa.render_check confirms duration, resolution and audio presence.

## Failure conditions

- Delivering 30.4 seconds for a 30 second contract.
- Two promises fighting for eight seconds.
- Grading the pack toward a mood that changes the brand colour.
- Super text added as a baked asset that cannot be localised or corrected.
- Music out of sync with the cut grid after a last-minute trim.
- Stating a performance claim the brief did not substantiate.

## Worked example

A 30 second spot for a hand tool. Import 22 product and 14 lifestyle clips, probe the hero pack shot at 1080p25, and pull two frames to lock the approved orange. Create "Anvil 30s" at 25 fps. Hero pack move 0.000-4.000 on the grid; talent proof 8.000-14.200 cut on the turn of the tool; benefit shot 14.200-20.000; supers centred at 9.000 for 2.4 seconds; end card from 26.000 with pack, URL and 3.5 seconds of stillness. Voice normalised, music at -14 dB with a 1 second fade. Export CRF 16 h264 mp4, then confirm with cinvar.qa.render_check at expected 30.0 seconds and resolution 1920x1080.

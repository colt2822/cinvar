---
name: podcast
title: Podcast Edit
summary: Turn a multi-track interview into a tight episode: silence removal, clean speech levels, chapter markers and a captioned audiogram.
tags: [podcast, interview, audio, dialogue]
---

## Objective

Deliver an episode that respects the listener's time: no dead air, no repeated sentence, dialogue that stays intelligible at low volume, and chapter structure the listener can navigate by. Speech is the product; everything else supports it.

## Creative principles

- Cut by meaning, not by waveform. A 0.4 second pause before a punchline stays.
- Keep both speakers at the same perceived level; the listener should not touch the volume control.
- Remove filler and false starts, but keep breath between sentences where it sounds human.
- If a guest repeats the question, cut the question.
- Cover every editorial cut with a visual change in video episodes, or accept the jump cut and make it clean.
- A cold open is one sentence from the strongest moment, then the title, then the show.
- Chapters are an edit decision, not metadata added afterwards: pick the points where the topic actually changes.
- Never process the voice beyond what intelligibility needs. Compression and EQ are not a substitute for good levels.

## Planning sequence

1. Read the project and confirm the toolchain before importing hour-long files.
    cinvar.project.status {}
    cinvar.system.capabilities {}

2. Import the camera and the recorder tracks separately so audio can be chosen independently.
    cinvar.media.import {"path": "/footage/ep41/cam_a.mov"}
    cinvar.media.import {"path": "/footage/ep41/recorder.wav"}

3. Probe both. Confirm sample rate, channels and whether the recorder has drift against the camera.
    cinvar.media.inspect {"path": "/footage/ep41/recorder.wav"}

4. Measure loudness before deciding any gain. Do not guess.
    cinvar.audio.analyze {"asset_id": "as_rec41"}
    cinvar.audio.clipping {"asset_id": "as_rec41"}

5. Find silence, then decide what to keep rather than what to cut.
    cinvar.audio.silence_plan {"asset_id": "as_rec41", "noise_db": -38, "min_secs": 0.9, "keep_secs": 0.2}

6. Build the timeline at the camera frame rate so the video track stays in sync.
    cinvar.timeline.create {"name": "EP41", "width": 1920, "height": 1080, "fps": "25"}

7. Place the clean recorder audio as the master and mute the camera audio.
    cinvar.timeline.insert_clip {"asset_id": "as_rec41", "at": "00:00:00.000"}
    cinvar.timeline.clip_audio {"clip_id": "cl_camaudio", "muted": true}

8. Cut the cold open from the strongest 12 seconds, then hard cut to the top of the show.
    cinvar.timeline.insert_clip {"asset_id": "as_rec41", "at": "00:00:00.000", "source_in": "00:41:12.000", "source_out": "00:41:24.000"}

9. Remove the accumulated silence in one ripple pass per section, then review each cut.
    cinvar.timeline.split {"clip_id": "cl_rec41", "at": "00:14:03.000"}
    cinvar.timeline.remove {"clip_id": "cl_rec41_b", "ripple": true}

10. Trim the head and tail of the whole episode to the first and last useful word.
    cinvar.timeline.trim {"clip_id": "cl_rec41", "source_in": "00:00:03.400"}

11. Normalise only where measured gain differs by more than 2 dB.
    cinvar.timeline.clip_audio {"clip_id": "cl_guest2", "gain_db": 3.0, "normalize": true}

12. Add a chapter card at each topic change.
    cinvar.timeline.text {"text": "02:14 - Finding the first hire", "at": "00:02:14.000", "duration": "3.0", "size": 44, "color": "white", "position": "lower_third"}

13. Captions for the audiogram and for accessibility.
    cinvar.timeline.captions {"cues": [{"start": "0.0", "end": "3.4", "text": "Most people get this backwards."}], "language": "en", "size": 46, "position": "bottom"}

14. Export audio-first, verify, then export the video cut.
    cinvar.render.export {"output": "/renders/ep41-master.mp4", "codec": "h264", "crf": 20, "format": "mp4", "burn_captions": false}

## Timing guidance

- Cold open: 10-18 seconds, one complete thought, no music sting longer than 1 second.
- Inter-speaker gap: 0.5-0.9 seconds feels natural; under 0.3 sounds clipped, over 1.4 sounds abandoned.
- Silence removal: cut spans over 0.9 seconds down to 0.2 seconds of retained air.
- Chapter cards: 2.5-3 seconds, no animation longer than 0.3 seconds.
- Music bed under speech: -20 to -24 dB relative to voice; swell only in gaps.
- Episode target: -16 LUFS integrated, true peak below -1.5 dBTP.

## Evaluation checklist

- No span of dead air longer than 0.9 seconds remains.
- Both speakers measure within 2 dB of each other.
- No repeated sentence, false start or audible edit click at any cut.
- Chapter points match the actual topic changes.
- Captions are complete and timecode-accurate at the start and end of the episode.
- cinvar.qa.render_check reports duration and audio presence for the exported file.
- Loudness was measured, not assumed.

## Failure conditions

- Normalising every clip to zero instead of measuring first, which flattens dynamics.
- Cutting to waveform zero crossings only, leaving half sentences.
- Leaving camera audio unmuted under the recorder track, producing comb filtering.
- Adding chapters by timestamp arithmetic without listening.
- Exporting at the wrong sample rate and then blaming the player.
- Reporting an episode length without checking the exported file.

## Worked example

A 62-minute interview recorded on camera and a separate recorder. Import both, probe the WAV at 48 kHz stereo, and find the recorder is 1.2 dB quieter than the camera. Mute the camera audio, keep the recorder as master, analyse loudness at -19 LUFS integrated, and raise the whole clip 3 dB. Run silence_plan with a -38 dB floor and remove 6 minutes 40 seconds of dead air, keeping 0.2 seconds around each exchange. Trim the head to 00:00:03.400. Place a cold open from 00:41:12-00:41:24 at the top, then the body. Add six chapter cards at the measured topic changes. Caption the full track, export at CRF 20, then verify with cinvar.qa.render_check at expected 54:20.

---
name: gaming
title: Gaming Highlight Edit
summary: Turn raw gameplay into a readable highlight: context first, action in the middle, punch-ins on impact and audio that never buries the game.
tags: [gaming, highlights, action, montage]
---

## Objective

Make gameplay footage readable to someone who did not play the session. The viewer must know the objective, the stakes and the outcome of each clip before the action peak arrives. Pace is earned by clarity, not by cutting faster.

## Creative principles

- Give one second of context before every play: the HUD, the position, or the score.
- The peak of the action gets the longest hold. Cut around it, never through it.
- A punch-in on impact is stronger than a shake effect. Scale up 1.12-1.25 for 4-8 frames.
- Keep the game audio dominant; music is a bed, not a replacement for cues.
- Do not cut mid-reload, mid-jump or mid-animation unless the missing frames are the joke.
- Group clips by tension, not by chronology, unless the run is a continuous story.
- Kill counts, ranks and timers on screen must be real and readable at 1080p.
- End the montage on the strongest single play, not on a fade to a logo.

## Planning sequence

1. Read the project state and open a session timeline per session.
    cinvar.project.status {}

2. Import the recording folder.
    cinvar.media.import_folder {"path": "/footage/ranked_march", "recursive": true}

3. Probe the captures. Console captures are often variable frame rate; note it before you trust a timecode.
    cinvar.media.inspect {"path": "/footage/ranked_march/session14.mp4"}

4. Find the action automatically, then verify every hit by eye.
    cinvar.media.scenes {"asset_id": "as_g14a77", "threshold": 0.28}
    cinvar.media.frames {"asset_id": "as_g14a77", "times": ["04:12.0", "04:14.5", "04:17.0"], "width": 960}

5. Build a proxy at 720p: long desktop recordings make previews expensive.
    cinvar.media.proxy {"asset_id": "as_g14a77", "height": 720}

6. Create the timeline at the capture frame rate so motion stays smooth.
    cinvar.timeline.create {"name": "Highlights March", "width": 1920, "height": 1080, "fps": "60"}

7. Place the context shot one second before each play.
    cinvar.timeline.insert_clip {"asset_id": "as_g14a77", "at": "00:00:00.000", "source_in": "00:04:11.000", "source_out": "00:04:21.500"}

8. Mark the impact frame, split there, and speed-ramp only the approach.
    cinvar.timeline.split {"clip_id": "cl_play01", "at": "00:00:04.200"}
    cinvar.timeline.speed {"clip_id": "cl_play01_a", "speed": 0.6}

9. Punch in on the peak with a short scale keyframe pair.
    cinvar.timeline.keyframe {"clip_id": "cl_play01_b", "property": "scale", "at": "00:00:04.200", "value": 1.0, "easing": "ease_in"}
    cinvar.timeline.keyframe {"clip_id": "cl_play01_b", "property": "scale", "at": "00:00:04.400", "value": 1.18, "easing": "ease_out"}

10. Add a short impact flash only where the action is genuinely loud.
    cinvar.timeline.effect {"clip_id": "cl_play01_b", "kind": "brightness", "params": {"value": 0.14}}

11. Duck the music under the action audio.
    cinvar.timeline.clip_audio {"clip_id": "cl_music", "gain_db": -16.0, "fade_in": "0.15", "fade_out": "0.4"}

12. Label the score or rank with a short overlay, once per run.
    cinvar.timeline.text {"text": "RANK 12", "at": "00:00:00.500", "duration": "2.0", "size": 52, "color": "#ffe066", "position": "top", "outline": true}

13. Preview the first 15 seconds, because a montage lives or dies there.
    cinvar.render.preview {"range_start": "0", "range_end": "15"}
    cinvar.render.status {"job_id": "job_c220"}

## Timing guidance

- Context before each play: 0.8-1.5 seconds.
- Setup-to-impact: 2.5-6 seconds. Longer than 8 seconds without an action beat loses the viewer.
- Peak hold: 0.4-1.0 second at normal speed, never skipped.
- Punch-in: 4-8 frames in, hold 6-12 frames, return over 4 frames.
- Clip-to-clip gap: 0-3 frames on a hard cut; music-driven montages may breathe 0.3 seconds.
- Music fades under gameplay audio: 0.15 seconds down, 0.4 seconds up.

## Evaluation checklist

- A viewer who never played can explain the objective and outcome of each clip.
- The impact frame of every play is on screen at full speed.
- Game audio is 5-10 dB above the music bed at all times.
- Text overlays do not cover the HUD element that carries the information.
- No black frames or loading screens between highlights.
- Frame rate matches the capture; no duplicated-frame stutter in the export.
- cinvar.qa.render_check confirms the export duration and audio track.

## Failure conditions

- Cutting every play at the same 3-second length, which flattens tension into a list.
- Adding screen shake or colour flashes to more than one clip in five.
- Ignoring area-dependent frame rates and delivering stuttering motion.
- Music louder than the game at the moment of the play.
- Scraping an old score or rank off a screenshot and presenting it as this run.
- Claiming a highlight is unbeaten without any source.

## Worked example

Four Ranked matches, 71 minutes total. Import, probe session14 at 1080p60 variable frame rate, build a 720p proxy, then detect scenes at 0.28 and keep 11 candidates after reviewing frames. Create the timeline at 60 fps. Open with one second of HUD context, cut to the first engagement, split at the impact frame, slow the approach to 0.6, punch in to 1.18 over 6 frames, return to 1.0. Repeat nine times, ending on the strongest play held for a full second. Duck music 16 dB under every engagement. Add one rank overlay at 00:00:00.500. Preview the first 15 seconds, export CRF 18 h264 at 60 fps, and verify with cinvar.qa.render_check.

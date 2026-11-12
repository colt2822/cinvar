---
name: video-restoration
title: Video Restoration Edit
summary: Repair legacy footage honestly: measure black frames, clipping and grain first, apply only supported fixes, report the rest.
tags: [restoration, archival, legacy, cleanup]
---

## Objective

Improve the watchability of old or damaged footage without inventing detail that was never recorded. Measure the defects, apply the corrections the toolchain actually supports, and state plainly what remains broken. A restored clip that looks plausible but is not the original is a failure.

## Creative principles

- Measure before touching anything. Every correction must answer a number you measured.
- Correct one defect at a time, in this order: geometry, levels, colour, then audio.
- Never crop to hide a defect that the archive needs to keep visible.
- Interlacing, dropouts and tape artefacts are evidence of age; remove noise, not context.
- Preserve the original. Restore into a new timeline; never overwrite the source asset.
- Do not upscale beyond what the source resolution supports and call it restored.
- Keep a documented record of every correction in the edit, because someone will ask.
- When a defect needs a tool this build does not have, say so instead of simulating it.

## Planning sequence

1. Read the project state and inventory the source before importing duplicates.
    cinvar.project.status {}

2. Import the source once, then never touch the file again.
    cinvar.media.import {"path": "/archive/reel_1978_telecine.mov"}

3. Probe the source: resolution, frame rate, duration, audio sample rate.
    cinvar.media.inspect {"path": "/archive/reel_1978_telecine.mov"}

4. Detect black spans; head and tail leaders are normal, mid-reel black is damage.
    cinvar.media.black {"asset_id": "as_arch01", "min_secs": 0.5}

5. Sample frames across the reel to judge grain, contrast and colour cast before choosing a correction.
    cinvar.media.frames {"asset_id": "as_arch01", "times": ["00:00:30.000", "00:04:00.000", "00:09:15.000"], "width": 960}

6. Build a proxy for review so the full reel can be scrubbed without waiting on every render.
    cinvar.media.proxy {"asset_id": "as_arch01", "height": 540}

7. Create a restoration timeline at the source resolution to avoid an early, irreversible resample.
    cinvar.timeline.create {"name": "Reel 1978 Restore", "width": 720, "height": 576, "fps": "25"}

8. Place the full reel, then trim the leader and tail rather than cutting into the content.
    cinvar.timeline.insert_clip {"asset_id": "as_arch01", "at": "00:00:00.000", "source_in": "00:00:08.000", "source_out": "00:11:42.000"}

9. Correct levels with a measured brightness change, not a guessed look.
    cinvar.timeline.effect {"clip_id": "cl_arch01", "kind": "brightness", "params": {"value": 0.08}}

10. Smooth excessive grain in the flattest areas only, and check that faces keep their texture.
    cinvar.timeline.effect {"clip_id": "cl_arch01", "kind": "blur", "params": {"sigma": 0.6}}

11. Remove damaged black at the head of the timeline so the piece starts on picture.
    cinvar.timeline.split {"clip_id": "cl_arch01", "at": "00:00:00.400"}
    cinvar.timeline.remove {"clip_id": "cl_arch01_a", "ripple": true}

12. Measure audio before correcting it; archival audio is often both quiet and clipped.
    cinvar.audio.analyze {"asset_id": "as_arch01"}
    cinvar.audio.clipping {"asset_id": "as_arch01"}
    cinvar.timeline.clip_audio {"clip_id": "cl_arch01", "gain_db": 2.0, "normalize": true, "fade_in": "0.3", "fade_out": "0.6"}

13. Review three representative minutes, then export a high-bitrate master and keep the source untouched.
    cinvar.render.preview {"range_start": "0", "range_end": "180"}
    cinvar.render.status {"job_id": "job_4410"}

14. Validate the delivered file against the timeline before calling it restored.
    cinvar.qa.render_check {"path": "/renders/reel_1978_restored.mp4", "expected_duration": 694.0}

## Timing guidance

- Review sample: 60-180 seconds spread across the reel before committing a correction.
- Leader trim: remove all but 8-12 frames of leader so the age of the material still reads.
- Effect onset: apply brightness and blur for the whole reel, not per shot, unless the splice itself changes stock.
- Audio fades: 0.3 seconds in, 0.6 seconds out; preserve room tone under speech.
- Export: CRF 14-16 for an archival master; CRF 20 or higher is for review copies only.
- Frame rate: keep the source rate. Converting 25 to 30 duplicates frames and is not a restoration.

## Evaluation checklist

- Every correction is traceable to a measurement taken in this session.
- The source asset is unchanged and the restoration lives in a separate timeline.
- Levels corrected without crushing blacks or clipping highlights.
- Grain reduced without turning faces into plastic.
- Audio measured before gain was applied, and clipping reported rather than hidden.
- Interlacing or unsupported defects are documented, not silently ignored.
- cinvar.qa.render_check passes on the delivered file.

## Failure conditions

- Applying a look before measuring black level, then calling the result restored.
- Cropping the frame to hide edge damage.
- Using blur to hide compression artefacts until the picture loses all detail.
- Upscaling and describing the output as higher resolution restoration.
- Overwriting the original asset or exporting into the archive folder.
- Reporting a defect as fixed when no tool in this build can address it.

## Worked example

An 11 minute 45 second telecine reel at 720x576, 25 fps, with 900 milliseconds of black at the head and visible grain. Import, probe, run black detection, and pull three frames to inspect cast and contrast. Build a 540p proxy, create a 720x576 timeline at 25 fps, place the reel with source_in 08.000 and source_out 11:42.000. Split at 00:00:00.400 and ripple-remove the leader. Raise brightness by 0.08 across the reel, apply 0.6 sigma blur to reduce grain without flattening faces, analyse audio at -27 LUFS with no clipping, raise 2 dB and normalise. Preview 180 seconds, export at CRF 15 h264, then run cinvar.qa.render_check with expected 694.0 seconds. Report the remaining vertical scratch as unfixable in this build.

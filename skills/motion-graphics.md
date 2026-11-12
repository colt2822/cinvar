---
name: motion-graphics
title: Motion Graphics Edit
summary: Animate type, shapes and stills with keyframes: one motion idea per element, easing that matches weight, and timing on a fixed grid.
tags: [motion-graphics, typography, keyframes, branding]
---

## Objective

Animate type, logos and stills so that movement explains hierarchy rather than decorating it. Each element has one entrance, one optional emphasis and one exit. Timing is set on a grid so the piece can be conformed to any length without re-authoring.

## Creative principles

- One motion idea per element. If a title scales, it does not also slide, blur and rotate.
- Animate what the viewer must read first. Motion is attention, and attention is a budget.
- Easing carries weight: ease_out for anything entering, ease_in for anything leaving, linear only for continuous loops.
- Keep type still long enough to be read: motion is not a substitute for hold time.
- Align to a grid and hold it. Elements that drift by two pixels read as sloppy.
- Stagger related elements by 2-4 frames; more than that reads as a delay, not a rhythm.
- Transparency is a last resort; a wipe or a mask reads better than a fade on a solid background.
- Every animated element must survive being exported at half frame rate without losing its meaning.

## Planning sequence

1. Read the project state and the delivery canvas requirements.
    cinvar.project.status {}

2. Confirm what the machine can render before promising a look.
    cinvar.system.capabilities {}

3. Import stills, logo files and any rendered 3D elements.
    cinvar.media.import {"path": "/assets/brand/logo_lockup.png"}
    cinvar.media.import {"path": "/render/title_3d_v2.mp4"}

4. Probe rendered elements: alpha channel, resolution and duration decide how they can be composited.
    cinvar.media.inspect {"path": "/render/title_3d_v2.mp4"}

5. Create the timeline at the final frame rate; animate at 30 or 60, never at a rate you will not deliver.
    cinvar.timeline.create {"name": "Brand Opener", "width": 1920, "height": 1080, "fps": "30"}

6. Place the base layer first so later elements animate over a known background.
    cinvar.timeline.insert_clip {"asset_id": "as_bg_grad", "at": "00:00:00.000", "source_in": "0", "source_out": "8.0"}

7. Add the primary title as text, not as an image.
    cinvar.timeline.text {"text": "CINVAR", "at": "00:00:01.000", "duration": "4.0", "size": 128, "color": "#f5f3ef", "position": "center", "outline": false}

8. Set the entrance. Text clips support animate_in / animate_out (fade, slide_up, slide_down) and x, y and opacity keyframes; they do NOT support scale or rotation keyframes (the tool refuses rather than ignoring them). Fastest: set it when you create the title.
    cinvar.timeline.text {"text": "CINVAR", "at": "00:00:01.000", "duration": "4.0", "size": 128, "color": "#f5f3ef", "position": "center", "outline": false, "animate_in": "slide_up", "animate_out": "fade"}
    For pictures, logos and footage use presets: cinvar.timeline.animate {"clip_id": "cl_logo", "preset": "pop"} (also fade_in, fade_out, fade_in_out, slide_in_left/right/up/down, push_in, pull_out, pan_left/right, drift).

9. Stagger the subtitle 3 frames behind the title with a y keyframe pair (positive y moves down; offsets are in timeline pixels and scale with the render size).
    cinvar.timeline.keyframe {"clip_id": "cl_sub", "property": "y", "at": "00:00:01.100", "value": 24.0, "easing": "ease_out"}
    cinvar.timeline.keyframe {"clip_id": "cl_sub", "property": "y", "at": "00:00:01.600", "value": 0.0, "easing": "ease_out"}

10. Add a controlled blur only where speed needs to be felt, and remove it as soon as the element lands.
    cinvar.timeline.effect {"clip_id": "cl_title", "kind": "blur", "params": {"sigma": 4}}

11. Use a slow scale on stills to keep them alive without a second motion idea.
    cinvar.timeline.keyframe {"clip_id": "cl_photo", "property": "scale", "at": "00:00:04.000", "value": 1.0, "easing": "linear"}
    cinvar.timeline.keyframe {"clip_id": "cl_photo", "property": "scale", "at": "00:00:08.000", "value": 1.06, "easing": "linear"}

12. Sequence-check the first three seconds at full speed and at half speed.
    cinvar.render.preview {"range_start": "0", "range_end": "3.0"}
    cinvar.render.status {"job_id": "job_31ab"}

13. Export with a low CRF so gradients and type edges stay clean.
    cinvar.render.export {"crf": 14, "codec": "h264", "format": "mp4", "burn_captions": false}

## Timing guidance

- Entrance: 6-12 frames. Anything slower than 18 frames reads as a fade.
- Exit: 6-10 frames, and never longer than the entrance.
- Type hold after entrance: minimum 24 frames at 30 fps, 40 frames for a full sentence.
- Stagger between related elements: 2-4 frames.
- Loop or ambient motion: 0.5-1.5 percent scale drift over 4 seconds; anything faster competes with the message.
- Whole opener: 5-9 seconds for a brand ident, 3-5 for a lower third animation.

## Evaluation checklist

- Each element has exactly one motion idea.
- Entrances use ease_out, exits use ease_in, loops use linear.
- Text is still long enough to be read at delivery speed.
- No element drifts off the grid by more than one pixel.
- Blur and brightness effects are gone by the time the element is at rest.
- Gradients show no banding at delivery bitrate.
- cinvar.qa.render_check confirms duration and resolution of the export.

## Failure conditions

- Stacking scale, rotation, blur and opacity on one element and calling it polish.
- Animating a logo so it never holds still.
- Using linear easing for an entrance, which reads as mechanical.
- Delivering type as a flattened image so it cannot be corrected.
- Exporting a gradient at CRF 28 and shipping visible banding.
- Claiming a frame-accurate duration without checking the exported file.

## Worked example

A seven second brand opener. Import the logo lockup and a 3D title render with alpha. Probe the render at 1920x1080, 30 fps, 4 seconds. Create the timeline at 30 fps, place a gradient background for 7 seconds, then the 3D render at 1.000. Add the wordmark as text at 1.500 with opacity 0 to 1 over 8 frames and scale 0.96 to 1.0 over 12 frames, easing ease_out. Stagger the subtitle 3 frames later on y from 24 to 0. Add a 4 sigma blur on the entry only, cut at 1.667. Slow-scale the closing still from 1.0 to 1.06 over the last 4 seconds. Preview 0-3 seconds, export at CRF 14, verify with cinvar.qa.render_check at expected 7.0 seconds.

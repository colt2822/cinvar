---
name: blender-animation
title: Blender Animation Integration
summary: Render a 3D logo reveal or title with Blender, then composite it: pick a template, match frame rate, hold the settle.
tags: [blender, 3d, title, logo]
---

## Objective

Produce a short 3D element with the built-in Blender templates and composite it into the edit so that lighting, background and frame rate match the surrounding footage. If Blender is missing or disabled, stop and report the dependency instead of faking the asset.

## Creative principles

- Treat the 3D render as a shot, not as an effect: it needs a real duration, a real background and a real hold at the end.
- Match the frame rate and resolution before rendering. A 24 fps render inside a 30 fps timeline will judder no matter how good the animation is.
- One accent colour per piece. It should already appear somewhere in the edit.
- Render with a background that matches what will sit behind it, or with transparency so the composite can be controlled later.
- Hold the final frame for at least 12 frames. A logo that never settles looks unfinished.
- Keep the camera move simple: one push, one rotation, or one reveal. Two moves fight.
- Never re-render to fix a timing problem that a trim can solve.

## Planning sequence

1. Read project state and confirm the timeline settings the render must match.
    cinvar.project.status {}

2. Check that Blender is installed before planning any 3D element.
    cinvar.blender.status {}

3. Render the element with an explicit template, text, duration and accent colour.
    cinvar.blender.animation {"template": "logo_reveal", "text": "NORTH COAST", "duration": "4.0", "color": "#e0b070"}

4. Import the rendered clip into the project so it becomes a normal asset.
    cinvar.media.import {"path": "/project/cache/blender/logo_reveal-4s.mp4"}

5. Probe the render: confirm resolution, frame rate and whether an alpha channel survived encoding.
    cinvar.media.inspect {"path": "/project/cache/blender/logo_reveal-4s.mp4"}

6. Create or reuse the delivery timeline at the frame rate the render was produced at.
    cinvar.timeline.create {"name": "Opener 3D", "width": 1920, "height": 1080, "fps": "30"}

7. Place the render on the timeline with an explicit in point at the first settled frame of the animation.
    cinvar.timeline.insert_clip {"asset_id": "as_blend01", "at": "00:00:00.000", "source_in": "00:00:00.200", "source_out": "00:00:03.800"}

8. Trim the tail so the last frame holds rather than cutting on a moving camera.
    cinvar.timeline.trim {"clip_id": "cl_blend01", "source_out": "00:00:03.800"}

9. Extend the hold with a freeze frame when the piece needs more air than the render provides.
    cinvar.timeline.freeze {"clip_id": "cl_blend01", "at": "00:00:03.700"}

10. Composite a title under or over the render when the template leaves the sub-line empty.
    cinvar.timeline.text {"text": "a short film", "at": "00:00:02.400", "duration": "2.6", "size": 34, "color": "#f2efe9", "position": "center", "outline": false}

11. Tune the render's contrast only if it does not match the surrounding grade.
    cinvar.timeline.effect {"clip_id": "cl_blend01", "kind": "brightness", "params": {"value": 0.06}}

12. Preview the transition in and out of the 3D element, not the element alone.
    cinvar.render.preview {"range_start": "0", "range_end": "6.0"}
    cinvar.render.status {"job_id": "job_8812"}

13. Export and verify resolution, duration and audio presence.
    cinvar.render.export {"crf": 16, "codec": "h264", "format": "mp4", "burn_captions": false}

## Timing guidance

- Logo reveal render: 3-5 seconds including a 0.5 second settle.
- Title animation: 4-7 seconds when it carries a subtitle, 2.5-4 when it is a single word.
- Hold on the settled frame: 12 frames minimum, 24 frames for a broadcast ident.
- Cut into the 3D element on motion, never on a static frame at the start.
- Trim in-point: skip the first 4-8 frames of a render, which are usually the camera's dead start.
- Accent colour: one, used for no more than 15 percent of the frame at any moment.

## Evaluation checklist

- Blender availability was checked before the render was promised.
- Rendered resolution and frame rate match the timeline exactly.
- The last frame holds for at least 12 frames.
- No frame in the render shows the default grey world or an unlit surface.
- The accent colour appears elsewhere in the edit.
- The composite shows no visible edge, halo or alpha fringe.
- cinvar.qa.render_check confirms the export matches the timeline duration.

## Failure conditions

- Rendering before checking cinvar.blender.status, then reporting an asset that does not exist.
- Ignoring a disabled Blender integration. The permission error is the answer; do not fake the render.
- Compositing a 24 fps render into a 30 fps timeline without conforming.
- Cutting directly from the settled logo to black with no hold.
- Re-rendering repeatedly to fix what a 4-frame trim would have fixed.
- Describing a rendered element as approved by a client or brand owner.

## Worked example

A six second opener for a 30 fps delivery. Call cinvar.blender.status and confirm Blender at /usr/bin/blender. Render logo_reveal with text "NORTH COAST", duration 4.0 and colour #e0b070. Import the result, probe it at 1920x1080 30 fps without alpha, and place it at 0.000 with source_in 0.200 and source_out 3.800. Freeze the settled frame at 3.700 for 0.6 seconds. Add a subtitle at 2.400 for 2.6 seconds, 34 px, centred. Raise brightness by 0.06 to match the surrounding grade. Preview 0-6 seconds, export at CRF 16 h264, then confirm with cinvar.qa.render_check at expected 6.0 seconds.

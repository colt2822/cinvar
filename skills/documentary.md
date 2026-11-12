---
name: documentary
title: Documentary Edit
summary: Shape interview-led documentary: pick the story spine first, cover cuts with real evidence, keep every claim attributable.
tags: [documentary, interview, archival, b-roll]
---

## Objective

Build a factual sequence from interviews, observational footage and archival material. The edit must be honest: every spoken claim belongs to the person who made it, every cut is covered by real footage, and nothing is presented as verified when the source simply says it. Emotion is earned by structure, not by music.

## Creative principles

- Find the spine in the transcripts before you open the timeline. Six statements beat two hundred clips.
- A-roll carries meaning; B-roll carries place and time. Do not cover a cut with footage that contradicts what is being said.
- Keep the pause before a hard answer. It is the most honest material you have.
- Attribution is part of the picture: identify speakers on first appearance, and on every archival item.
- Music enters after the point is made, never before it.
- Do not manufacture continuity between shots that were taken hours apart; use a title card instead.
- If the footage does not support a claim, cut the claim, not the footage.
- Leave the subject's contradictions in. A clean, consistent story is usually a rewritten one.

## Planning sequence

1. Read project state and inventory before importing a shooting day.
    cinvar.project.status {}

2. Import interviews and observational footage into separate folders so the two never mix in the bin.
    cinvar.media.import_folder {"path": "/footage/doc1/interviews", "recursive": false}
    cinvar.media.import_folder {"path": "/footage/doc1/observational", "recursive": true}

3. Probe every interview track: audio channels, sample rate, and whether the recorder was running.
    cinvar.media.inspect {"path": "/footage/doc1/interviews/subject_a.mov"}

4. Pull contact sheets before watching hour-long files end to end.
    cinvar.media.contact_sheet {"asset_id": "as_int_a1", "count": 16, "columns": 4}

5. Find usable shot material in observational footage.
    cinvar.media.scenes {"asset_id": "as_obs_c3", "threshold": 0.30}

6. Create the timeline at the interview camera's frame rate.
    cinvar.timeline.create {"name": "Doc Cut 1", "width": 1920, "height": 1080, "fps": "25"}

7. Lay the selected interview statements in story order with source in and out points that start on the breath, not the syllable.
    cinvar.timeline.insert_clip {"asset_id": "as_int_a1", "at": "00:00:00.000", "source_in": "00:12:41.000", "source_out": "00:13:02.500"}

8. Check every cut for a jump: if the framing changes within the same answer, cover it with evidence.
    cinvar.timeline.split {"clip_id": "cl_int01", "at": "00:00:09.200"}
    cinvar.timeline.insert_clip {"asset_id": "as_obs_c3", "at": "00:00:09.200", "source_in": "00:03:10.000", "source_out": "00:03:14.400"}

9. Remove dead air between answers, keeping enough room for the silence to read as thought.
    cinvar.audio.silence_plan {"asset_id": "as_int_a1", "noise_db": -36, "min_secs": 1.1, "keep_secs": 0.35}

10. Identify speakers and archival sources with lower thirds.
    cinvar.timeline.text {"text": "Maria Alvarez, site supervisor", "at": "00:00:02.000", "duration": "4.0", "size": 38, "color": "white", "position": "lower_third", "template": "lower_third"}

11. Level interview sound against location sound, which is usually 6-10 dB hotter.
    cinvar.audio.analyze {"asset_id": "as_obs_c3"}
    cinvar.timeline.clip_audio {"clip_id": "cl_obs03", "gain_db": -7.0, "normalize": true}

12. Add music only after a section lands, and keep it under speech.
    cinvar.timeline.clip_audio {"clip_id": "cl_score1", "gain_db": -22.0, "fade_in": "1.2", "fade_out": "1.5"}

13. Preview a full section to test whether the argument holds without captions.
    cinvar.render.preview {"range_start": "0", "range_end": "00:04:30"}
    cinvar.render.status {"job_id": "job_d091"}

## Timing guidance

- Statement length: 4-12 seconds. A single answer longer than 20 seconds needs an internal visual change.
- Cut on the breath: 2-4 frames before the first syllable, 4-8 frames after the last.
- Silence kept after a hard answer: 0.8-2.0 seconds. Do not fill it.
- B-roll covers: 2.5-5 seconds each, and never the same geography twice in a row.
- Lower third: 3-5 seconds, legible at 1080p, never over a face's eyeline.
- Music beds under speech: -20 to -24 dB relative to the voice.

## Evaluation checklist

- Every claim on the track is attributable to a speaker or an identified archive.
- No cut is covered by footage from a different place or season without a title card saying so.
- Speaker identification appears on first appearance and on every archive item.
- Location audio never jumps more than 3 dB between adjacent shots.
- The spine survives a listen with the picture off.
- No fabricated reenactment is presented as original footage.
- cinvar.qa.project_check reports no missing assets before the cut is called done.

## Failure conditions

- Writing the script first and then trimming statements until they say it.
- Covering a jump cut with unrelated B-roll, which viewers read as a lie about time.
- Music pushed to the front of an argument to do the work the evidence should do.
- Combining two separate answers into a sentence the subject never said.
- Unlabelled archival footage.
- Reporting a factual conclusion the footage does not support.

## Worked example

Three interviews and one shooting day. Import both folders, probe each interview, and build contact sheets. Select 14 statements totalling 7 minutes 40 seconds, ordered as: the problem, the failed attempt, the decision, the cost. Place them with in and out points on breaths. Five statements have framing jumps inside one answer; cover each with observational footage from the same location and date, labelled by a date card when the season differs. Keep 1.2 seconds of silence after the failed-attempt answer. Identify four speakers and two archive sources. Level location audio down 7 dB, add a score bed at -22 dB that starts after the decision statement. Preview four minutes, export, verify with cinvar.qa.render_check.

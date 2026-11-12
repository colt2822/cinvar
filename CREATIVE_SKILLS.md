# Creative skills

Tools explain what operations exist. Skills explain how to use them well.

A skill is a Markdown file with YAML frontmatter. It is data, not code: you can read it,
edit it, copy it, share it and override it without recompiling CINVAR.

## Where skills come from

Load order (later overrides earlier by name):

1. `$CINVAR_SKILLS_DIR` when set;
2. `/skills` in the working directory;
3. `~/.config/cinvar/skills`;
4. `<directory of the cinvar binary>/skills`.

## Format

    ---
    name: short-form
    title: Short-form vertical editing
    summary: Hooks, vertical reframing and captions for 15-60 second clips.
    tags: [vertical, captions, social]
    version: 1
    ---

    ## Objective
    ## Creative principles
    ## Planning sequence
    ## Timing guidance
    ## Evaluation checklist
    ## Failure conditions
    ## Worked example

Only these frontmatter keys are read: name, title, summary, tags, tools, version.
Everything after the closing `---` is the body, which is what the agent sees.

## The shipped library

| skill | for |
|---|---|
| cinematic | shot continuity, pacing, mood, transitions, colour, sound design |
| youtube | intros, retention pacing, B-roll, cut timing, captions |
| short-form | vertical reframing, hooks, rapid cuts, readable captions |
| podcast | speaker transitions, multicam cutting, silence removal, clip extraction |
| gaming | highlight selection, reactions, speed ramps, comic timing |
| documentary | narration structure, supporting footage, restrained treatment |
| commercial | product presentation, branding, concise messaging |
| motion-graphics | titles, keyframes, overlays, animated compositions |
| blender-animation | 3D scenes, camera moves, lighting, render hand-off (needs Blender) |
| video-restoration | denoise, colour correction, stabilisation, upscaling |

Each skill names real CINVAR tools in its planning sequence, so an agent can follow it
literally.

## How the agent uses them

The system prompt lists installed skills with a one-line summary. The agent calls
`cinvar.skills.load` with the names it needs, and only those bodies enter the context. That
is why the library can be large without costing tokens every turn.

## Writing your own

Put a file in `~/.config/cinvar/skills/my-style.md`, then check it loaded:

    cinvar skills list
    cinvar skills show my-style

Keep guidance concrete: timings in seconds, transition names that exist in CINVAR, tool
sequences a model can execute, and evaluation criteria a machine can check. A skill that
says "make it feel premium" without saying what to do is not useful to a model. Skills
never override the honesty rules in the system prompt.

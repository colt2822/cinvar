# CINVAR skills

Skills are Markdown files that teach the editing agent how to do a specific kind of
job: what to aim for, which tools to call in which order, how long shots should run,
what to check before calling a cut finished, and what makes the result a failure.

They are guidance, not code. Nothing in a skill executes on its own; the agent loads
one and follows it with the tool surface described in the file.

## Repository layout

    skills/
      README.md            this file, never loaded as a skill
      cinematic.md
      youtube.md
      short-form.md
      podcast.md
      gaming.md
      documentary.md
      commercial.md
      motion-graphics.md
      blender-animation.md
      video-restoration.md

The file name and the frontmatter name must match: cinematic.md declares name: cinematic.

## Format

A skill is Markdown with YAML frontmatter delimited by --- on its own line. The body is
everything after the closing ---.

    ---
    name: cinematic
    title: Cinematic Narrative Edit
    summary: Build a restrained narrative sequence from long footage.
    tags: [cinematic, narrative, long-form]
    version: 1
    tools: cinvar.media.import, cinvar.timeline.insert_clip
    ---

    ## Objective
    ...

Supported keys, and nothing else:

| key     | required | meaning                                                        |
| ------- | -------- | -------------------------------------------------------------- |
| name    | no       | skill id; falls back to the file name; lowercase, digits, - _ . |
| title   | no       | display title; falls back to name                               |
| summary | no       | one-line description used in the skill index; keep it under 140 |
| tags    | no       | list as [a, b] or comma separated; stored lowercase             |
| tools   | no       | free-text list of tool names the skill uses; accepted, ignored  |
| version | no       | free-text version marker; accepted, ignored                     |

Malformed frontmatter is an error that names the file: a missing opening ---, a closing
--- that never arrives, a line that is not key: value, or an unknown key. A malformed
file is skipped with a warning on stderr and never stops the rest of the load.

The parser is hand-rolled; there is no YAML dependency. Comments starting with # and
blank lines inside the frontmatter are allowed. Single or double quotes around a value
are stripped.

Write the body with these headings, in this order. The agent relies on them:

    ## Objective
    ## Creative principles
    ## Planning sequence
    ## Timing guidance
    ## Evaluation checklist
    ## Failure conditions
    ## Worked example

## Installing a custom skill

Put the file in the user skills directory:

    ~/.config/cinvar/skills/my-look.md

On Linux and macOS the directory follows XDG: if XDG_CONFIG_HOME is set the path is
$XDG_CONFIG_HOME/cinvar/skills, and CINVAR_CONFIG_DIR overrides both. On Windows the same
relative layout is used under the user profile.

A minimal custom skill:

    ---
    name: my-look
    title: My House Style
    summary: A two-camera interview look with fixed cut lengths.
    tags: [house-style, interview]
    ---

    ## Objective
    Keep every answer in a fixed 6 second rhythm.

    ## Creative principles
    - Cut only on the guest's breath.

    ## Planning sequence
    1. cinvar.project.status {}
    2. cinvar.media.import {"path": "/footage/interview_a.mov"}

    ## Timing guidance
    - Answers 6-9 seconds.

    ## Evaluation checklist
    - No cut lands mid-word.

    ## Failure conditions
    - Cutting to a tempo the speaker does not have.

    ## Worked example
    A 9 minute interview cut to 7 minutes 20 seconds, all cuts on breath.

Then run cinvar skills list. No registration step is needed.

## How discovery and overrides resolve

CINVAR_SKILLS_DIR, when set and non-empty, replaces the whole search path: only that
directory is read. That is the switch to use for tests, examples and demos.

Otherwise the loader merges these directories, in increasing precedence:

1. ./skills in the current working directory: the repository builtin set.
2. skills/ next to the cinvar executable: the installed set, if a packager ships one.
3. ~/.config/cinvar/skills: the user directory.

Merging is by name, not by path. When two directories contain a skill with the same
name, the later directory wins completely: title, summary, tags and body all come from
the winning file. That is how a user overrides a builtin without editing the repository:
drop a cinematic.md with the same name into ~/.config/cinvar/skills and it replaces the
shipped one.

Directories that do not exist are ignored. Unreadable files are skipped with a warning
on stderr; one bad file never fails the load. README.md and dotted files are never
treated as skills.

## How the agent loads skills

The agent sees skill names and summaries in its system prompt and loads the bodies on
demand, so a large library costs nothing until it is used.

- cinvar.skills.list returns name, title, summary and tags for every installed skill.
- cinvar.skills.load with {"names": ["cinematic", "short-form"]} returns those bodies in
  full, plus an "not installed" entry for any name that does not exist.

The same two views are available from the CLI:

    cinvar skills list
    cinvar skills show cinematic

render_prompt in src/skills/mod.rs builds the compact form used when composing a system
prompt: with no selection it emits one line per skill (name and summary); with a
selection it emits only the selected bodies, truncated at the character budget on a line
boundary with an explicit truncation note. Unselected skills are never included.

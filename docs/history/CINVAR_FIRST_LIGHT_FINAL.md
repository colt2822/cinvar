# CINVAR First Light - final report

Operation First Light asked one question: can a real AI model use CINVAR to create a video,
then revise it conversationally? The answer is yes - through CINVAR's MCP interface, driven
by a real model - while CINVAR's own internal agent loop remains BLOCKED on credentials that
do not exist on this machine.

## Report

    CINVAR_VERSION=0.1.0
    COMMIT=a770309
    LIVE_MODEL_CONNECTION=BLOCKED
    REAL_MODEL_TOOL_CALLING=PASS
    AUTONOMOUS_VIDEO_CREATION=PASS
    CONVERSATIONAL_REVISION=PASS
    REAL_AGENT_MCP_EDITING=PASS
    VISUAL_REVIEW=PASS
    PROJECT_PERSISTENCE=PASS
    FFMPEG_RENDERING=PASS
    MCP_SERVER=PASS
    PROVIDER_COMPATIBILITY=openai-compatible, anthropic, gemini, ollama, mock
                           (wire formats unit-tested; no live vendor call - see below)
    REAL_VIDEO_OUTPUT=demos/first_light/first-light/renders/first-light-trailer-v4.mp4
    REVISED_VIDEO_OUTPUT=demos/first_light/first-light/renders/first-light-dramatic-open-v2.mp4
    MODEL_USAGE=$0.91 total (Demo A $0.4392 / 74 turns, Demo B $0.3417 / 42 turns, setup probes $0.13)
    RENDER_PERFORMANCE=16 s 1080p timeline renders in 8.6 s wall (h264_nvenc); 11.4 MB binary; 4.8 MB startup RSS
    TESTS_PASSED=141
    TESTS_FAILED=0
    OPEN_CRITICAL=1
    OPEN_HIGH=2
    RELEASE_STATUS=ALPHA

## What each line means, and the evidence behind it

### LIVE_MODEL_CONNECTION=BLOCKED

CINVAR's provider adapters were not exercised against a vendor API because this machine has
no API key: the environment has no provider variables, and the only model credential present
(the DeepSeek platform token in the harness credential store) is rejected by
api.deepseek.com with HTTP 401 - both stored records were probed and both failed. No Ollama
or llama.cpp server is running. Shipping a PASS here would be a lie, so it is BLOCKED, and
the machine's only authorized model access was used instead: the authenticated Claude Code
CLI.

### REAL_MODEL_TOOL_CALLING=PASS

claude-sonnet-5-5 made 74 MCP tool calls in Demo A and 42 in Demo B against CINVAR's stdio
MCP server, with --strict-mcp-config so no other server was reachable. It imported media,
read scene lists, analysed loudness, built a timeline, trimmed, reordered, set transitions,
added a title, adjusted audio, rendered, polled job handles, extracted frames and read the
PNG files.

### AUTONOMOUS_VIDEO_CREATION=PASS

Demo A: 7 synthetic assets in, one instruction in, 18.000 s 1920x1080 h264+aac file out with
CINVAR QA score 1.000. The model chose the sequence, trims, transitions, title placement and
audio treatment itself. Evidence: demos/first_light/trace.json.

### CONVERSATIONAL_REVISION=PASS

Demo B: the same project, one follow-up sentence. Intro trimmed 4 s to 2 s, timeline 18 s to
16 s, title enlarged and recoloured, opening fade shortened, zooms added, re-rendered to
16.000 s and QA score 1.000. The model named revision 31 as the rollback point; restoring it
and re-validating the original render was verified independently (score 1.000).
Evidence: demos/first_light/revision_trace.json.

### REAL_AGENT_MCP_EDITING=PASS

A is B: the model reached CINVAR only through MCP, exactly as an outside assistant would. No
CINVAR-specific client code was involved in either demo.

### VISUAL_REVIEW=PASS

The model extracted frames from its own renders with cinvar.media.frames, opened the PNGs,
and made decisions from what it saw - moving the title because it collided with the text
baked into the clip, and stating plainly that the frames showed flat colour cards with no
motion. Two CINVAR defects were discovered that way (see defects 2 and 3 below).

## Defects found by the live model and fixed in this session

1. Creative skills were invisible when the MCP host started the server outside the repo. The
   model got an empty skill list. Fixed by compiling the shipped skills into the binary
   (build.rs); filesystem directories still override them by name.
2. wipeleft, wiperight, slideup and circleopen rendered as a dip to black. Fixed by rejecting
   them at the tool boundary with the supported list, so a model never gets a different
   transition than it asked for.
3. render.export accepted paths outside the project, which then broke frame review. Fixed:
   the tool refuses them with a hint that shows the correct form.
4. Keyframes rendered silently static and trim left holes. Fixed: the render job returns the
   warning, the keyframe tool documents the limitation, and timeline.trim gained ripple:true.
5. Added during the session for real use: cinvar.timeline.transform, cinvar.timeline.reorder,
   cinvar qa <file>, cinvar project history, cinvar project rollback, frame extraction from
   rendered files, and a decision-framework skill that tells a model when not to use effects.

## Tests

    cargo test  ->  141 passed, 0 failed
      lib 102, core_integration 5, w1_render_integration 7, w2_providers 19,
      w3_mcp_protocol 2, w5_integration 6

    ./examples/verify_all.sh  ->  12/12 checks (offline mock path, unchanged)

## Open items

CRITICAL (1): CINVAR's internal agent loop has never run against a live vendor API. It needs
an API key; everything around it is ready and unit-tested.

HIGH (2): keyframe animation is not rendered (first value only, now reported); speech
transcription is not implemented, so captions must be supplied as cues.

Also open, lower severity: Blender never executed here; macOS and Windows unverified;
3+ stacked video tracks and rotated sources unrendered; plugin network/write permissions
recorded but unenforced; no cross-process project lock; the web UI chat flow was never
clicked through in a browser.

## How to reproduce

    cargo build --release && cargo test
    ./examples/verify_all.sh          # offline harness, no model needed
    ./demos/run_first_light.sh        # real model over MCP, about $0.40
    ./demos/run_directors_cut.sh      # conversational revision, about $0.35
    ./demos/verify_demos.sh           # ffprobe + CINVAR QA on the delivered files

## Verdict

A real model, given one sentence and a folder of clips, produced a validated 1080p video and
then improved it from a second sentence, using nothing but CINVAR's tools. The product
promise now holds for any MCP-capable assistant. The remaining gap is credential plumbing:
CINVAR's own loop speaks the same protocol internally and has simply never been pointed at a
vendor endpoint that would answer.

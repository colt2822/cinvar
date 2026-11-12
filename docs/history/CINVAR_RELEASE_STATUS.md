# CINVAR release status

## Update — Operation First Light (live model milestone)

    LIVE_MODEL_CONNECTION=BLOCKED   (no vendor API key exists here; DeepSeek platform token
                                     rejected with 401; no local model server running)
    REAL_MODEL_TOOL_CALLING=PASS    (claude-sonnet-5-5 made 74 + 42 MCP tool calls)
    AUTONOMOUS_VIDEO_CREATION=PASS  (18.000 s 1080p trailer, QA 1.000)
    CONVERSATIONAL_REVISION=PASS    (16.000 s revised cut, QA 1.000, rollback verified)
    REAL_AGENT_MCP_EDITING=PASS     (the model reached CINVAR only through MCP)
    VISUAL_REVIEW=PASS              (model read extracted frames and acted on them)
    TESTS_PASSED=186  TESTS_FAILED=0
    RELEASE_STATUS=ALPHA

Details, evidence and the defects the live model surfaced are in CINVAR_FIRST_LIGHT_FINAL.md
and demos/README.md. The block below is the previous (pre-First-Light) release record and is
kept for history; its NATURAL_LANGUAGE_EDITING=PARTIAL line referred to the offline planner.

---
This is the final report for this build session. Every number here comes from
CINVAR_TEST_RESULTS.md, which in turn comes from executing the software on the build machine.

    CINVAR_VERSION=0.1.0
    REPOSITORY=/path/to/cinvar
    COMMIT=0a9a01a (the commit holding this build's code; this status file follows it)
    CORE_HARNESS=PASS
    MODEL_PROVIDERS=openai-compatible, anthropic, gemini, ollama, mock (wire formats unit-tested;
                    mock exercised end to end; NO live vendor call was made - no credentials here)
    NATURAL_LANGUAGE_EDITING=PARTIAL
    TIMELINE_ENGINE=PASS
    FFMPEG_RENDERING=PASS
    MCP_SERVER=PASS
    PLUGIN_SYSTEM=PASS (fixture plugin; permission gating verified)
    CREATIVE_SKILLS=PASS
    PROJECT_MEMORY=PASS
    AUTOMATED_QA=PASS
    BLENDER_INTEGRATION=PARTIAL (script generation unit-tested and py_compile-checked;
                                no Blender installed, so no real 3D render was produced)
    WEB_UI=PARTIAL (HTTP endpoints verified with curl; the chat and export flow was not
                    clicked through in a browser)
    REAL_VIDEO_RENDER=PASS
    EXTERNAL_AGENT_TEST=PASS
    TESTS_PASSED=139
    TESTS_FAILED=0
    SUPPORTED_PLATFORMS=linux-x86_64 (verified). macOS and Windows: code paths and CI matrix
                        written, never executed.
    OPEN_SOURCE_READINESS=PARTIAL
    RELEASE_STATUS=ALPHA

## Why NATURAL_LANGUAGE_EDITING is PARTIAL, not PASS

The harness loop, the tool surface, the budgets, the transcript and the full
instruct -> import -> timeline -> title -> audio -> render -> poll -> QA sequence were all
executed for real, driven by the offline planner (`provider = "mock"`). A real language model
was never called because no provider credentials exist in this environment, and inventing a
result is not an option. With a key, the same loop runs against OpenAI-compatible,
Anthropic, Gemini or Ollama endpoints; the request/response wire formats are unit-tested
against the vendors' documented shapes but not against the vendors themselves.

To convert it: `/cinvar models add work --provider anthropic --model <model>`, `/cinvar models key work`,
`cinvar models test work`, then `cinvar run "..."`.

## What is incomplete

1. No live model run (above).
2. Speech transcription is not implemented - no backend is bundled, so captions must be
   supplied as cues. This is the largest missing feature relative to the product vision.
3. Per-clip audio ducking (`duck_under_speech`) is modelled but not applied; loudness
   normalisation is applied per clip and globally.
4. Blender has never rendered a scene here; the bridge is fixture-tested only.
5. macOS and Windows are unverified.
6. Three or more stacked video tracks, .mov/.mkv containers and rotated sources were not
   rendered.
7. Plugin `network` and `write_paths` permissions are recorded, not enforced; plugin
   timeouts were not exercised.
8. Projects have a cross-process lock (trusts a live pid on Linux) and stale-save refusal; the
   lock is untested on macOS/Windows. Render jobs are in-memory: a killed server loses the job id
   (partial file is cleaned, re-export needed).
9. No release artefacts are built or published, and the name CINVAR is provisional pending
   trademark clearance.
10. The web UI has no authentication; it binds to 127.0.0.1 only.

## Install and reproduce

    # dependencies
    sudo apt install ffmpeg            # or: brew install ffmpeg / winget install Gyan.FFmpeg

    cd ~/cinvar
    cargo build --release              # 54 s on this machine
    cargo test                         # 139 passed, 0 failed
    ./examples/verify_all.sh           # 12/12 end-to-end checks

    export CINVAR_CONFIG_DIR=~/.config/cinvar
    ./target/release/cinvar setup
    ./target/release/cinvar models add mock --provider mock --model mock-planner
    ./examples/make_footage.sh /tmp/footage
    ./target/release/cinvar project create --name Demo --dir /tmp
    ./target/release/cinvar --project /tmp/demo run "Combine the clips in /tmp/footage, add a title, and export a 1080p video"

    ./target/release/cinvar mcp serve --project /tmp/demo     # external agents
    python3 examples/mcp_client.py /tmp/demo
    ./target/release/cinvar ui --port 8420                    # local web UI

## Verdict

A working alpha: the editing engine, project format, renderer, QA, tool surface, MCP server,
skills library and CLI are real and verified. The one thing a user most wants to see - their
own model driving the studio - is implemented and untested against a live model, because this
environment has no credentials. That is the first thing to fix before calling this anything
more than alpha.

## Director's Room update (2026-11-10)
Keyframe animation, ducking, two-pass loudness, multitrack compositing fix, captions suite, project lock/atomic renders, MCP workflow guide, `cinvar connect claude`, QA black_flash. 184 tests pass / 0 fail. Status remains ALPHA. See CINVAR_DIRECTORS_ROOM_FINAL.md for per-area PASS/PARTIAL and limits. Items previously HIGH (keyframes not rendered) are closed; transcription remains unverified with a real engine.

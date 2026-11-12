# CINVAR test results

Everything in this file was produced by running the software in this repository on the
build machine. Nothing here is inferred from code inspection.

## Environment

    OS            Ubuntu 24.04.4 LTS, Linux 7.0.0-42-generic, x86_64
    CPU           12 cores
    GPU           RTX 3060 Ti (NVENC exercised) + Tesla P40 (not exercised)
    FFmpeg        6.1.1-3ubuntu5 with libx264, libx265, libvpx-vp9, aac, libopus,
                  h264_nvenc, hevc_nvenc, av1_nvenc, qsv and vaapi encoders
    Rust          1.98.1 (cargo 1.98.1)
    Blender       not installed
    Python        3.12.3 (used only by the example MCP client and one Blender-script syntax check)

## Test suite

Command:

    cd ~/cinvar && cargo test

    unittests src/lib.rs                  102 passed  0 failed
    unittests src/main.rs                   0 passed  0 failed
    tests/core_integration.rs               3 passed  0 failed
    tests/w1_render_integration.rs          7 passed  0 failed
    tests/w2_providers.rs                  19 passed  0 failed
    tests/w3_mcp_protocol.rs                2 passed  0 failed
    tests/w5_integration.rs                 6 passed  0 failed
    doc tests                              0 passed  0 failed
    TOTAL                                 139 passed  0 failed

What the integration tests actually do (not mocks):

- `w1_render_integration`: generates synthetic clips with FFmpeg, builds a project, appends
  clips, adds a drawtext title and an audio track, renders real MP4/WebM files, then probes
  them with ffprobe for duration, resolution, frame rate and codecs. It also covers the
  hardware-encode fallback and a real scene-detection pass on the rendered file.
- `w5_integration`: renders a real file and asserts QA passes; truncates that file to 100
  bytes and asserts a named QA check fails; asserts an empty file fails; asserts a black-lead
  clip fails `black_frames` with timestamps; exports OTIO and asserts the document shape plus
  the warnings for unmapped features; compiles the generated Blender Python with python3.
- `w3_mcp_protocol`: spawns the real binary and drives a full JSON-RPC session over stdio
  (initialize, tools/list, tools/call, malformed line, unknown method, unknown tool,
  resources, EOF), plus a plugin fixture that is refused without subprocess permission and
  executed when enabled.
- `w2_providers`: asserts the exact OpenAI/Anthropic/Gemini request bodies and response
  parsing, the retry classifier, error redaction, and the offline planner's staging.
- `core_integration`: asserts per-clip loudness normalisation reaches the filtergraph, that a
  normalised clip renders and passes QA, and that a job handle can be waited on.

## End-to-end verification

Command:

    ./examples/verify_all.sh

Output (12/12 checks):

    PASS  build.binary            target/release/cinvar
    PASS  doctor.tools            50 tools
    PASS  footage.synthetic       3 clips + audio
    PASS  project.create          <tmp>/verify
    PASS  agent.render            verify-r10.mp4 duration=15.000000
    PASS  agent.qa_verdict        planner saw a passing QA
    PASS  mcp.suite               external agent checks passed
    PASS  skills.loaded           10 skills
    PASS  otio.export             2578 bytes
    PASS  cli.status              project reported
    PASS  bench.measured          reported
    INFO  mcp checks passed: 20
    SUMMARY failures=0

The MCP suite (`examples/verify_mcp.py`) drives the server as an external agent would and
printed 20 passing assertions, including:

    PASS  mcp.initialize           {'name': 'cinvar', 'version': '0.1.0'}
    PASS  mcp.tools.list           50 tools
    PASS  revision.remove          {'ok': True, 'removed': 'clip_98ea92ae'}
    PASS  revision.duration_shrank 15.0s -> 11.0s
    PASS  render.status            succeeded progress=1.0
    PASS  qa.render_check          score=1.0
    PASS  revision.rollback        restored revision 11
    PASS  revision.rollback_restores_clips  three clips back
    PASS  vertical.timeline        {'width': 1080, 'height': 1920}
    PASS  vertical.dimensions      1080x1920
    PASS  vertical.qa              score=1.0

## Rendered artefacts

1080p timeline (three clips, title overlay, per-clip audio normalisation, 15 s):

    ffprobe format=duration=15.000000 size=~380000
    video: h264 1920x1080 30/1
    audio: aac 48000 Hz 2 ch

Vertical Short (1080x1920 canvas, two burned-in caption cues):

    ffprobe display_aspect_ratio=9:16 width=1080 height=1920
    video: h264, audio: aac, duration=5.000000, size=185364

Encoder actually used on this machine for the 1080p export: `h264_nvenc` (the encoder probe
verified NVENC before selecting it); the CPU `libx264` path is covered by the tests.

## Quality control

QA is deterministic and measured, not asserted. A real render:

    passed=true score=1.0
      file_exists    measured 356461 bytes (> 1024 required)
      video_stream   measured video stream h264 1920x1080
      resolution     measured against the timeline canvas
      duration       measured against the timeline duration
      audio_present  measured audio stream aac 48000 Hz 2 ch
      black_frames   measured black spans
      audio_levels   measured mean and max dB

The same file truncated to 100 bytes:

    passed=false score=0.0, video_stream "could not measure: moov atom not found"

## Measured performance (this machine, not claims)

     release binary size                       11.4 MB
     startup max RSS (cinvar --version)        4.8 MB release / 8.7 MB debug
     config load                               < 1 ms
     project save / load                       < 1 ms (small project)
     ffprobe of a 5 s clip                     60 ms
     synthetic 5 s 720p clip generation        302 ms
     12 s 1080p timeline render (NVENC)        6.9 s
     15 s 1080p timeline render (NVENC)        ~7 s (agent run including QA)

## Explicitly NOT verified

- No live language-model call. No provider credentials exist on this machine; the adapters
  are unit-tested against constructed wire bodies and the harness is proven with the offline
  planner. `cinvar models test <profile>` fails cleanly with an actionable message when a key
  is missing (provider_error naming the environment variable).
- Blender: not installed, so `render_animation` never rendered a real scene. The generated
  Python is asserted by unit tests and compiled with `python3 -m py_compile`.
- macOS and Windows: never executed. Their code paths exist and CI is configured.
- Speech transcription: not implemented.
- Speech ducking per clip (`duck_under_speech`) is modelled but not applied; global loudness
  normalisation is.
- Three or more stacked video tracks, .mov/.mkv containers and rotated sources were not
  exercised.
- Plugin timeout expiry and plugin network/write permissions are not enforced.
- `opentimelineio` library validation: the .otio file is schema-shaped JSON, not loaded by
  the official Python library (not installed).

## Reproducing

    git clone <repo> && cd cinvar
    cargo test
    ./examples/verify_all.sh
    ./examples/demo1_basic_editing.sh
    python3 examples/mcp_client.py /path/to/project

## Operation First Light — live model runs

These were executed after the release above, with a real model (claude-sonnet-5-5 through
the authenticated Claude Code CLI) driving CINVAR exclusively through its MCP server.

| Run | turns | wall | cost | output | QA |
|---|---|---|---|---|---|
| Demo A - First Light | 74 | 124.7 s | $0.4392 | 18.000 s 1920x1080 h264+aac, 806,644 bytes | score 1.000 |
| Demo B - Director's Cut | 42 | 86.1 s | $0.3417 | 16.000 s 1920x1080 h264+aac, 789,989 bytes | score 1.000 |

Evidence files: demos/first_light/trace.json, demos/first_light/revision_trace.json,
demos/README.md. Rollback was verified on a copy of the project: restoring revision 31 gives
back the 18 s / 7-clip timeline and QA of the original render against it scores 1.000.

Measured on this machine during this session:

    16 s 1080p timeline render (release binary, h264_nvenc)   8.6 s wall
    startup RSS (cinvar --version)                            4.8 MB
    release binary                                            11.4 MB
    model-cost per demo (74/42 turns)                         $0.44 / $0.34

Test suite after this session's changes: 141 passed, 0 failed
  (lib 102, core_integration 5, w1_render_integration 7, w2_providers 19,
   w3_mcp_protocol 2, w5_integration 6)

New coverage added: ripple trimming, per-clip loudness normalisation reaching the filtergraph,
render warnings for unsupported features, embedded skills loading from any working directory,
transition-name rejection, and render-output path containment.

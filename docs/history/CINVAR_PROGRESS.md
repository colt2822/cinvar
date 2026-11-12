# CINVAR build progress

Updated during the build session. This is the shared status record: what works, what does
not, and what is next. Status vocabulary is strict:

- **PASS** - implemented and verified by running it
- **PARTIAL** - implemented, some paths unverified
- **MOCKED** - works through the deterministic offline planner, not a language model
- **UNVERIFIED** - code exists, never executed in this environment
- **NOT IMPLEMENTED** - absent

## Current status

| Area | Status | Evidence |
|---|---|---|
| Repository, Cargo workspace, licence, CI | PASS | builds on this machine; CI matrix written |
| CLI surface | PASS | see CINVAR_TEST_RESULTS.md |
| Configuration + model profiles | PASS | config round-trip test |
| Credential store + redaction | PASS | redaction test |
| Project document, schema versioning, revisions, rollback, relink | PASS | store tests |
| Timeline operations | PASS | ops tests |
| Undo / redo | PASS | history test |
| SQLite persistence | PASS | db test |
| Background jobs with progress + cancellation | PASS | jobs tests |
| Tool registry shared by agent and MCP | PASS | tool count reported by doctor |
| Agent loop with budgets, truncation, transcripts | PASS through the offline planner | demo run |
| Model providers | PARTIAL | adapters written and unit-tested; no live key in this environment |
| Media analysis | PASS | real probes, scene/silence/black detection, contact sheets |
| Render backend | PASS | real MP4/WebM rendered and probed, NVENC + CPU |
| QA checks | PASS | 7 measured checks; fails a truncated file |
| MCP server | PASS | 20 external-agent assertions over a real stdio process |
| Plugin system | PASS | fixture plugin, permission gating refused when disabled |
| Creative skills | PASS | 10 skills, 65 KB of guidance, all parse and load |
| Blender bridge | UNVERIFIED | Blender is not installed here; script generation unit-tested |
| OpenTimelineIO export | PASS | 2578-byte document with warnings |
| Local web UI | PASS | serves page and JSON API over the same runtime |
| Installers | PARTIAL | scripts written; release asset path needs a release |
| macOS / Windows | UNVERIFIED | never executed here (Linux only) |

## Interfaces frozen during this build

    project::model    Project / Timeline / Track / Clip / Effect / Caption types (serde, schema v1)
    project::ops      every timeline mutation, all validated
    project::store    ProjectStore: create, open, save, snapshot, revisions, rollback, import_asset
    media             probe, extract_frame, contact_sheet, detect_scenes, detect_silence,
                      detect_black, extract_audio, make_proxy
    audio             analyze_loudness, detect_clipping, silence_plan
    render            RenderRequest, plan_render, render, validate_output, available_encoders
    qa                validate_render, validate_project
    providers         Provider trait, ChatRequest, ChatResponse, Message, ToolSchema, Usage
    tools             Registry, ToolSpec, ToolCtx; groups.rs defines the tool surface
    agent             AgentSession::send, EditPlan
    mcp               McpServer::serve_stdio
    plugins           PluginManifest, discover, register
    skills            Skill, load_all, render_prompt
    jobs              Jobs::spawn, get, list, cancel, wait; JobCtx progress and cancel

## Known gaps

- No live language-model run was performed: no provider credentials exist in this
  environment. The harness is proven with the offline planner, and the adapters are tested
  against constructed request and response shapes. NATURAL_LANGUAGE_EDITING is PARTIAL.
- Blender is not installed, so the 3D path is unverified end to end.
- Speech transcription is not implemented: no transcription backend is bundled, so captions
  must come from supplied cues. Marked NOT IMPLEMENTED in the release status.
- macOS and Windows are untested.
- Project files are protected by a cross-process lock and stale-save refusal (Linux verified);
  render job ids are not persisted across a server kill.

## Next actions

1. Run the full suite: cd ~/cinvar && cargo test
2. Run the demo: ./examples/demo1_basic_editing.sh
3. Drive the MCP server: python3 examples/mcp_client.py /path/to/project
4. Open the local UI: cinvar ui
5. With credentials: cinvar models add work --provider anthropic --model MODEL, then
   cinvar models test work and cinvar run "..." to convert NATURAL_LANGUAGE_EDITING to PASS.

## Operation First Light (live model milestone)

| Area | Status | Evidence |
|---|---|---|
| Real model driving CINVAR over MCP | PASS | demos/first_light/trace.json (74 turns, $0.44) |
| Autonomous video creation | PASS | 18.000 s 1080p trailer, QA 1.000 |
| Conversational revision | PASS | 16.000 s revised cut, QA 1.000, rollback verified |
| Visual review of rendered frames | PASS | model read PNGs and moved the title because of what it saw |
| CINVAR's internal agent loop vs a vendor API | BLOCKED | no API key on this machine (401 verified) |
| Creative skills available from any working directory | PASS | embedded via build.rs, 11 skills |
| Transition honesty | PASS | unsupported names rejected, not substituted |
| Render output containment | PASS | render.export refuses paths outside the project |
| Ripple trimming | PASS | timeline.trim {ripple:true}, tested |

New CLI surface this phase: cinvar qa <file>, cinvar project history, cinvar project rollback.
New tools: cinvar.timeline.transform, cinvar.timeline.reorder; media.frames now reads renders.
New skill: decision-framework (when not to use an effect).

Open: keyframe animation is now rendered (verified on pixels); transcription engine verified against a stub only;
Blender, macOS/Windows, 3+ stacked tracks, rotated sources unverified.

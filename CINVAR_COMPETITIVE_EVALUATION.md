# CINVAR Competitive Evaluation — Autonomous Editing (Sprint II)

Method: documented-capability research by web search on 2026-11-13 (research notes with per-claim source tags and URLs in `benchmarks/competitive-draft.md`), compared against CINVAR behaviour that was actually run and measured in this repo. **No competitor was installed, run, or benchmarked; every competitor claim below carries the draft's verification tags. Nothing here is a performance benchmark of competitors.**

## Documented autonomy landscape

| Product | Raw-footage → finished cut | Agent/MCP | Where AI runs | Self-QA / repair |
|---|---|---|---|---|
| CINVAR (measured) | Yes, deterministic, local (`cinvar auto-edit`, semantic instruction optional) | Yes, shipped MCP tools incl. `cinvar.ai.auto_edit`, `cinvar.scenes.*` | Fully local (vision: gemma3:4b via Ollama; ASR: faster-whisper; no credits) | Yes: QA render/project checks + bounded correction loop, corrections recorded |
| DaVinci Resolve | Not documented as one step; agent drives scripting API **[SECONDARY]** | ResolveMCP, Studio 21.1+ **[SECONDARY]** | Local Neural Engine, no credits **[SECONDARY]** | Not documented |
| Premiere Pro | No single-step mode; AI Assistant (beta) does bins/markers/first assembly **[DOC]** | MCP previewed, CC "early 2027" **[DOC]** | Generative features cloud, credits **[DOC]** | Not documented |
| CapCut | Auto Cut / script-to-video (marketing-qualified) **[DOC]** | none found | cloud (vendor) **[DOC]** | Not documented |
| Descript | Underlord (beta) edits from a plain-language goal **[DOC]** | MCP shipped **[DOC]** | Cloud, paid credits, **explicitly non-deterministic** **[DOC]** | Review/undo per change, not an autonomous loop **[DOC]** |
| Runway | Generative video-to-video (Aleph 2.0), not clip selection **[DOC]** | API, no MCP found | Cloud, credits **[DOC]** | n/a |
| OSS (auto-editor, FunClip, etc.) **[REPO]** | Pipeline tools: loudness/Whisper/LLM heuristics | mostly none | local options | not documented |

## Where CINVAR genuinely distinguishes itself (observable here)
1. **Deterministic + reproducible autonomy**: the same footage and brief always yield the same plan (unit-tested ordering; no LLM in the loop unless instructed). Descript's Underlord is documented non-deterministic and billed per run.
2. **Fully local**: vision descriptions, transcription, selection, and rendering all run on this machine with zero credits and zero network (Ollama + faster-whisper + FFmpeg). Competitors' comparable flows are documented cloud/credit-based.
3. **Self-inspection and bounded repair with evidence**: every correction names the defect, the evidence (QA detail), the timeline change, and re-verifies. No surveyed product documents an autonomous QA/repair loop.
4. **Provenance discipline**: scene descriptions are labelled `vision:<model>` vs `deterministic`; unmatched creative requests are refused ("no indexed footage matches") rather than hallucinated.

## Where CINVAR is behind (documented, not contested)
- Polished interactive editors: Resolve/Premiere/CapCut offer far richer manual editing, color, and effects tooling. CINVAR's studio is basic.
- Mature transcription UX: Resolve/Premiere/Descript word-level text-based editing and speaker detection are documented; CINVAR has clip-level transcription with visible base-model errors.
- Scale: Resolve runs in production at 8K+; CINVAR's 8K support is documented but not benchmarked this sprint.
- Distribution: these are shipped products with installers; CINVAR is a source alpha.

## Not claimed
- Time-to-finished-edit, cost, or output-quality comparisons against any competitor: none were measured, and we will not invent numbers.

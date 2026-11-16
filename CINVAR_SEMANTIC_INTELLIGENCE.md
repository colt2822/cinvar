# CINVAR Semantic Intelligence

## What exists
A local, deterministic-controllable semantic layer over raw footage:

- `cinvar analyze --media-root DIR --project DIR [--model gemma3:4b]` — segments footage on scene cuts, samples a frame per segment, and describes it with a LOCAL vision model through Ollama (`http://localhost:11434`). Also the MCP tool `cinvar.scenes.index`.
- Segment index fields: source file, timestamps, description, keywords, subjects, mean audio dB, scene-cut density, 0..1 activity, provenance (`vision:<model>` or `deterministic`), confidence.
- `cinvar scenes "query"` / MCP `cinvar.scenes.search` — free-text retrieval over the index. No hits = no matching footage exists; the system refuses to invent media.
- `cinvar auto-edit --instruction "..."` — blends semantic relevance into clip selection: 0.6 × heuristic (cuts + audio) + 0.4 × semantic match, and switches selection from round-robin to global best-score so the edit serves the brief.

## Honesty rules (enforced, not aspirational)
- Model descriptions are tagged `vision:<model>`; fallbacks are tagged `deterministic` with confidence 0.3 and say "No vision model available" in the description itself. Stubbed AI is never presented as understanding.
- The deterministic pipeline remains fully usable and is the default (no instruction → no model dependency).

## Verified evidence (all local inference, real video)
- Model: `gemma3:4b` via Ollama on the local GPUs. (`moondream` was tried first and produced garbage output ("!!!") on this stack — documented, replaced.)
- Index of three labeled synthetic clips (readable on-frame text: CELEBRATION/TEAM WINS, ACTION CHASE, CALM FOREST WALK): gemma3 correctly read the frame text in all three descriptions.
- Search "celebration team wins" → celebration.mp4 at 1.00. "fast chase action" → action.mp4. "delicious pizza" → honest `no indexed footage matches`.
- Selection acceptance (labeled reference = celebration.mp4):
  - heuristic-only: celebration score 0.37, equal footing with the rest (round-robin);
  - with instruction "celebration team wins": celebration boosted to 0.60 and selected FIRST; unrelated calm.mp4 dropped from the reel entirely (12 s target).
  - opposite instruction "calm suspense" boosts calm.mp4 to 0.50 — the system demonstrably follows different briefs over the same footage.
- MCP: `cinvar.scenes.index` and `cinvar.scenes.search` verified over the stdio transport (initialize → tools/call), 76 tools exposed.

## Provenance of descriptions
Every segment records which system produced its description; `cinvar scenes` output prints `[vision:gemma3:4b]` or `[deterministic]` per row.

## Limitations
- One mid-segment frame per segment (motion/dialogue summarization not done).
- Keyword search is lexical (no embeddings); works well on concrete subjects.
- No speaker/emotion analysis; no repeated-footage detector yet.

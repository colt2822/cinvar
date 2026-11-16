# Competitive draft: autonomous / AI editing capabilities

Scope: autonomous and AI editing only (auto-edit from raw footage, clip selection, auto-captions, self-QA/repair, agent/MCP integration). Manual editing features are out of scope.

Method and limits:
- Research was done by web search plus a handful of page extracts on 2026-11-13. Most vendor pages were not opened in full; several claims come from secondary aggregators (Voyager, PCMag, GitHub awesome-lists). Each claim is tagged below.
- Tags: **[DOC]** = stated on a vendor's own page or docs seen in search/extract. **[SECONDARY]** = from a third-party review or aggregator. **[REPO]** = from a GitHub README/listing, not run locally.
- Nothing here was run or benchmarked locally. No competitor performance numbers are given, because none were found in primary sources.
- Items marked "not verified" could not be checked against a primary source in this pass.

---

## DaVinci Resolve (Blackmagic Design)

Documented autonomous/AI capabilities:
- Transcription and text-based editing: transcribes clips and lets you edit the timeline by editing the transcript; speaker detection added in v19. **[DOC]** https://documents.blackmagicdesign.com/SupportNotes/DaVinci_Resolve_19_New_Features_Guide.pdf ; **[DOC]** https://www.blackmagicdesign.com/media/release/20240606-03
- IntelliScript: builds a timeline from a text script by matching it to transcribed takes (added in v20). **[SECONDARY]** https://me.pcmag.com/en/video-editing/14278/davinci-resolve (PCMag), https://voyager.so/resources/apps/davinci-resolve
- Scene cut detection, IntelliSearch, Smart Reframe, Magic Mask (v2 since 20), AI animated subtitles, Voice Isolation, Dialogue Separator, Speech Generator. Studio-only per the Resolve 21 manual as reported by Voyager. **[SECONDARY]** https://voyager.so/resources/apps/davinci-resolve
- Silence removal and keyframe markers from transcript words. **[SECONDARY]** PCMag
- Explicitly **not** automatic: PCMag notes Resolve "can't automatically remove filler words (like 'um' and 'ah')" (review of an earlier version; not re-verified).
- Scripting API (Studio): timelines, media pool, markers, render queue, transcription, subtitle creation. Subtitle text and styling cannot be edited via script per Voyager. **[SECONDARY]** Voyager
- Official MCP server ("ResolveMCP"), Studio 21.1 and later, configurable for Claude Desktop, Claude Code, Codex, and others. **[SECONDARY]** Voyager (read from vendor announcement per its own footer; not verified directly here)
- Headless rendering with `-nogui`. **[SECONDARY]** Voyager (says Blackmagic states scripting keeps working headless; not verified)
- Voyager's job-by-job table rates 9 of 16 common agent jobs "yes", 5 "partly", 2 "no". **[SECONDARY]** Voyager. Treat as an aggregator's assessment, not a benchmark.

Pricing (as reported): free edition; Studio is a one-time $295 or $30/month cloud rental. Scripting from outside the app, the MCP server, and most AI tools require Studio. **[SECONDARY]** Voyager (verify on Blackmagic's pricing page before publishing)

Observable limits:
- Most AI tools are Studio-only; the free edition has far fewer.
- No autonomous "give it footage, get a cut" flow is documented. An agent must drive the scripting API step by step.

---

## Adobe Premiere Pro

Documented autonomous/AI capabilities:
- Generative Extend (Firefly, cloud): adds up to 2 s of video or up to 10 s of audio to a clip end; also generates missing transition frames. Dialogue is muted during extension. Requires internet. **[DOC]** https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/generative-extend-overview.html and https://helpx.adobe.com/premiere/desktop/edit-projects/edit-with-generative-ai/generative-extend-faq.html
- Speech to Text, Text-Based Editing (edit timeline via transcript), find and replace in transcript, remove all instances of one speaker in transcript, auto captions, caption translation, Enhance Speech. Speech/Text-Based Editing listed as ongoing development. **[DOC]** same overview page; https://helpx.adobe.com/content/help/en/premiere/desktop/edit-projects/edit-with-generative-ai/add-frames-using-generative-extend.html
- Auto Reframe (sequences and clips). **[DOC]** helpx index as seen in search
- AI Assistant panel (public beta, announced June 18, 2026): sorts assets into bins, batch renames clips, identifies interview questions, adds markers, "assembling a working starting point." **[DOC]** https://news.adobe.com/news/2026/06/adobe-unveils-major-expansion ; https://helpx.adobe.com/premiere/desktop/whats-new/whats-new.html (lists the AI Assistant panel in the November 2026 version 27.0 notes)
- Generative Media tool: generates video, sound effects, soundscapes, music into the timeline (Nov 2026 announcement). **[DOC]** https://blog.adobe.com/en/publish/2026/11/10/explore-new-ways-create-across-creative-cloud-adobe-firefly-while-advancing-craft-precision-choice-control
- MCP integrations for Premiere, previewed, with the Creative Cloud rollout stated as "early 2027." **[DOC]** same Adobe blog post above. Not available now.
- Premiere mobile: "Quick Cut" to generate an editable first cut from a prompt, and an AI Assistant to create Shorts or highlights, in private beta. **[DOC]** https://blog.adobe.com/en/publish/2026/11/10/adobe-firefly-premiere-mobile-ways-create-videos-edit-content-perfect-soundtrack-stories

Pricing and access:
- Generative Extend is a "premium" generative feature, accessible through Firefly standalone plans and select Creative Cloud plans. Basic assistive AI (Speech to Text, Caption Translation, Enhance Speech) is in the CC membership. **[DOC]** Generative Extend FAQ
- Cloud-based generative features need internet.

Observable limits:
- Generative Extend is capped at 2 s video / 10 s audio per use.
- AI Assistant is in public beta; the MCP story is not shipped.
- Adobe's own docs describe the assistant as operating "in the Project panel or Timeline." No documented "raw footage to finished cut" autonomous mode was found.

---

## CapCut (ByteDance)

Documented autonomous/AI capabilities:
- Auto Captions / Recognise Subtitles: speech-to-text to timed caption blocks, editable, exportable as burn-in or as editable/sidecar text. Available on Web, Desktop, Mobile. **[DOC]** https://www.capcut.com/resource/auto-captions-ai-video-editor ; **[DOC]** https://www.capcut.com/tools/video-keywords (auto-highlight keywords option)
- Auto Cut: "automatically trims and syncs your clips to music beats, speech pauses, or text prompts." **[DOC]** https://www.capcut.com/help (search snippet)
- Script-to-video: enter a script, get generated clips, images, background music, with a scene breakdown. **[DOC]** https://www.capcut.com/resource/capcut-ai ; https://www.capcut.com/resource/how-to-create-ai-video
- "Auto adjust" for lighting, color, and contrast across clips. **[DOC]** https://www.capcut.com/resource/capcut-ai (vendor marketing copy; "automatically" is not qualified)
- AI Video Maker: generates a full script from a topic and a video from the script. **[DOC]** https://www.capcut.com/resource/how-to-create-ai-video

Observable limits:
- CapCut's help center states that direct project import/export from Premiere Pro is not supported. **[DOC]** https://www.capcut.com/help/how-to-export-pro-project (search snippet)
- Standard vs Pro subscription terms changed in 2026; device coverage is restricted. **[DOC]** CapCut help (search snippet)
- Output is social-format oriented.

Not verified: the exact accuracy of Auto Cut and Auto Adjust. No independent test was run.

---

## Descript

Documented autonomous/AI capabilities:
- Text-based editing: the transcript is the timeline; delete words to delete media. **[DOC]** https://www.descript.com/features ; https://www.descript.com/video-editing
- Underlord (AI co-editor, beta): given a raw recording and a plain-language goal, performs edits. Documented tasks include: captions and caption styling, clipping (split into max-length clips; find social clips of 60–90 s and reformat vertical), animations and zoom cuts, translation of subtitles, bulk edits (center speakers, bleep profanity, lower thirds), slides-to-video, web search for facts. Removing filler words, shortening word gaps, and "edit for clarity" are AI tools in the panel. **[DOC]** https://help.descript.com/getting-started/underlord-beta-your-ai-co-editor-in-descript.md
- Underlord uses paid AI credits for the "agent brain" plus each tool it runs; cost varies run to run; it is non-deterministic. **[DOC]** same help page (explicitly stated)
- Underlord can write a script, read a script for feedback, and generate B-roll and avatar-hosted videos. **[DOC]** https://www.descript.com/underlord
- Studio Sound, Eye Contact correction, Regenerate, translation, remote recording. **[DOC]** https://www.descript.com/features
- Descript MCP: connects Claude or another agent to edit, transcribe, clip. Runs on the public developer API. **[DOC]** https://www.descript.com/features ; https://help.descript.com/api-and-mcp/mcp.md (linked; not opened)
- Also: Zapier integration and public API. **[DOC]** help.descript.com (linked from Underlord page)
- Model selector: Underlord lets the user choose among several third-party LLMs (GPT, Claude, Gemini, Grok names as listed on the help page). **[DOC]** help page. Premium models require a paid plan.

Observable limits:
- The help page explicitly describes Underlord as non-deterministic and billed per run. Expect variance between runs.
- Underlord is marked beta on the vendor's help page.

Not verified: the output quality of any Underlord task. No test was run.

---

## Runway

Documented capabilities (API):
- Aleph 2.0 (`aleph2`), a video-to-video model. Text-prompted edits to an input video (2–30 s, at least 480p, 30 fps or lower). Modes: `reference`, `extend`, `edit`. Keyframe images at up to 5 timestamps, ranged edits applied only inside a time window, output up to 1080p with original audio. **[DOC]** https://docs.dev.runwayml.com/_llms-txt/core-api.txt ; https://dev.runwayml.com/learn/edit-and-finish-video-api
- Finishing endpoints: upscale to 4K, frame-rate change, SDR to HDR, dialogue cleanup/dub (per the Runway Dev guide). **[DOC]** https://dev.runwayml.com/learn/edit-and-finish-video-api
- Aleph 2.0 changes "what is in the picture" without mask/tracking by the user, according to the guide. **[DOC]** same. This is vendor-stated; not verified locally.
- Gen-4 Aleph (`gen4_aleph`) and Gen-3 Alpha Turbo are no longer available on Runway Dev API as of July 30, 2026. **[DOC]** https://docs.dev.runwayml.com/_llms-txt/core-api.txt (changelog). A third-party cheatsheet notes the Academy price table still lists Gen-4 Aleph for app users at 15 credits/s. **[SECONDARY]** https://www.ai-mindset.com/runway-cheatsheet

Observable limits:
- Input length and resolution constraints are documented (above).
- Credit pricing is per second for app users; check current pricing before publishing.
- Runway is a generative model for editing, not a timeline or clip-selection tool. It does not document automatic clip selection from long footage.
- Not verified: whether Aleph 2.0 consistently preserves identity or motion. Vendor claim only.

---

## Open-source AI / automation tools (non-exhaustive)

All of these are from GitHub listings or READMEs. **[REPO]** unless noted. None were installed or run.

- **auto-editor** (WyattBlue, Unlicense, ~5.8k stars per listing): cuts silence and "dull" parts by audio loudness, exports to Premiere Pro, DaVinci Resolve and Final Cut Pro via timeline interchange. Written in Nim. https://github.com/WyattBlue/auto-editor . **Autonomous, rule-based, not AI.** Clip selection is loudness-based, not semantic.
- **MoviePy** (Zulko, MIT): Python scripting for cuts, concatenation, titles, effects. Programmatic, no built-in AI. https://github.com/Zulko/moviepy
- **whisperX** (m-bain): Whisper with word-level timestamps and speaker labels, for caption timing. https://github.com/m-bain/whisperX
- **openai/whisper**: base speech recognition; output can be turned into subtitles. https://github.com/openai/whisper
- **FunClip** (modelscope): transcribes video, generates subtitles, clips by text selection. https://github.com/modelscope/FunClip
- **AI-Youtube-Shorts-Generator** (MIT): Whisper transcription, LLM highlight selection, face-tracked 9:16 crops. Listed in clipx-io/awesome-ai-clipping. Pipeline claim only.
- **autoclip** (zhouxiaoka, MIT): cuts long videos into clips with subtitles; Chinese-language docs. https://github.com/zhouxiaoka/autoclip
- **AutoClip-MVP** (Farkoal2128): Windows local-first; AI highlight suggestion via Ollama or cloud LLM; human review step; burned-in captions. Notes that cloud providers receive transcript text. https://github.com/Farkoal2128/AutoClip-MVP
- **ai-video-captions** (nicolaigaina, MIT): Whisper + FFmpeg captions, six animated styles, Docker. Listed in zhangwenhao66 list.
- **vibeclip** (oktaydbk54, AGPL-3.0): self-hosted, turns long videos into captioned 9:16 shorts, "edit by chatting," BYO LLM key. Listed; not verified.
- **CapRocket** (medhachim): Electron/React/FFmpeg desktop editor with offline Whisper captions and an AI assistant. License field says "no license" on the listing, which matters for reuse.
- **EverythingAI-Pro/ai-video-editor**: claims auto-cut filler, auto-place B-roll, auto-caption, shorts; "edit by conversation." License "other." Claims not verified.

Observable pattern across OSS: the common autonomous stack is Whisper (or WhisperX) for transcripts, an LLM for highlight selection, and FFmpeg/MoviePy for rendering. Quality, speed, and reliability were not measured.

---

## Cross-product observations (no numbers)

- Autonomy level, as documented:
  - Fully prompt-driven agent that edits in place: Descript Underlord (beta), Premiere AI Assistant (public beta, scope limited to bins, markers, first assembly), Premiere mobile Quick Cut (private beta).
  - Scriptable/agent-drivable but not autonomous by itself: DaVinci Resolve (scripting + ResolveMCP, Studio only).
  - Per-feature AI, user drives: CapCut auto captions, Premiere Generative Extend, Resolve Magic Mask.
  - Generative clip/shot tool, not an editor: Runway Aleph 2.0 (video-to-video).
  - Rule/loudness-based autonomy: auto-editor. Pipeline-based: most OSS clippers.
- Agent/MCP integration:
  - Descript: MCP shipped (documented).
  - DaVinci Resolve: ResolveMCP shipped, Studio 21.1+ (secondary source).
  - Premiere: MCP previewed, "early 2027" for Creative Cloud (documented, not shipped).
  - CapCut, Runway: no MCP found in this pass. Runway has an API; not MCP-checked.
- Cloud vs local:
  - Resolve: AI runs locally on the DaVinci Neural Engine, no credits (secondary).
  - Premiere Generative Extend: cloud, internet required (documented).
  - Descript Underlord and Runway: cloud, paid credits (documented).
  - OSS: local options exist (Whisper, Ollama), with cloud LLM options.
- Self-QA / repair: no product in this pass documented an autonomous self-check or repair loop. Descript's Underlord says you can review or undo each change. Premiere AI Assistant says "you stay involved." Not a verified feature of any product; treat as a gap in the research, not a finding.

---

## Claims I could not verify locally

- All vendor capability claims above were read from search snippets or extracts, not tested.
- Voyager's job-by-job table, Resolve MCP details, and Resolve headless behavior are secondary sources.
- Runway Aleph 2.0 quality claims are vendor-stated.
- OSS claims are README/listing claims; no repo was cloned or run.
- Pricing and plan terms change; check each vendor's pricing page before publishing.

## Gaps to close before publishing

- Open Descript's MCP and API docs (help.descript.com/api-and-mcp/*) and Resolve's ResolveMCP docs directly.
- Check the Adobe Premiere AI Assistant docs page directly (scope, beta limits).
- Run one reproducible test per product on the same clip, if you want observable comparisons. Keep the results separate from the documented claims.

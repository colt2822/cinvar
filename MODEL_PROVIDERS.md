# Model providers

CINVAR talks to models through one trait (`providers::Provider`). Everything the model
needs to edit video is exposed as tools, so switching providers changes the brain, not
the editing environment.

## Supported providers

| provider value | Endpoint | Tool calling | Images | Notes |
|---|---|---|---|---|
| `openai` | https://api.openai.com/v1 | yes | yes | chat completions + function calling |
| `openai-compatible` | your `base_url` | yes | depends | any /v1/chat/completions server |
| `deepseek` | https://api.deepseek.com/v1 | yes | no | OpenAI-compatible tool calling |
| `xai` / `grok` | https://api.x.ai/v1 | yes | yes | OpenAI-compatible |
| `openrouter` | https://openrouter.ai/api/v1 | yes | model dependent | routes to many models |
| `anthropic` | https://api.anthropic.com/v1 | yes | yes | native Messages API, tool_use blocks |
| `gemini` / `google` | generativelanguage.googleapis.com | yes | yes | native generateContent, functionDeclarations |
| `ollama` | http://localhost:11434 | model dependent | model dependent | local inference |
| `llamacpp` / `local` | your `base_url` | model dependent | no | llama.cpp server, OpenAI-compatible |
| `mock` | none | yes | no | offline deterministic planner for demos and CI |

Capability profiles are honest per provider. A text-only model still edits video: the
harness returns inspection results as JSON (scene lists, loudness numbers, contact sheet
paths) that any model can reason about. Multimodal models get the same data plus image
paths.

## Configuration

Configuration lives in `~/.config/cinvar/config.toml` (override the directory with
`CINVAR_CONFIG_DIR`).

    version = 1
    active_model = "local"

    [models.local]
    provider = "ollama"
    model = "qwen2.5:7b"
    base_url = "http://localhost:11434"
    context_window = 32768
    max_output_tokens = 4096
    temperature = 0.2

    [budget]
    max_iterations = 30
    max_model_calls = 60
    max_tokens = 400000
    max_task_seconds = 3600
    max_usd = 5.0

    [security]
    allow_network = true
    allow_media_upload = false
    allow_plugin_subprocess = false
    allow_blender = true

CLI equivalents:

    cinvar models add work --provider anthropic --model claude-sonnet-4 --api-key-env ANTHROPIC_API_KEY
    cinvar models use work
    cinvar models test work
    cinvar models list
    cinvar models key work
    cinvar models remove work

## Credentials

Resolution order for a profile named `work`:

1. the environment variable named by `api_key_env` in the profile;
2. `CINVAR_API_KEY`;
3. the conventional provider variable (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
   `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `XAI_API_KEY`, `OPENROUTER_API_KEY`);
4. `~/.config/cinvar/credentials.toml` (mode 0600).

Keys are never written to project files, transcripts, logs or error messages. Every message
passes through redaction before it is shown or stored.

## Behaviour under load

- Requests time out rather than hang; connection and read timeouts are bounded.
- 429 and 5xx responses retry up to three times with 1s/2s/4s backoff.
- Response bodies are capped so a misbehaving endpoint cannot exhaust memory.
- Usage is accounted per turn and stored in the local database.
- Budgets are enforced by the agent loop: iterations, model calls, tokens, wall-clock time
  and a nominal dollar ceiling. Exhausting one returns a `budget_exceeded` error naming it.

## The offline mock planner

`provider = "mock"` runs a deterministic planner with no network access. It performs a
real edit through the real tools: project status, media import, timeline creation, clip
assembly, title, audio normalisation, render, render polling and QA. Its final message says
"offline mock planner" so a reader can never mistake it for a language model.

It exists so CI, demos and first-run evaluation do not require credentials. It is not an
intelligence and it does not adapt to arbitrary instructions.

## Adding a provider

Implement the trait, advertise capabilities honestly, and register the provider name in
`providers::build`. See CONTRIBUTING.md.

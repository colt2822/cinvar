# RunPod cloud rendering (EXPERIMENTAL - mock-tested only)

**No live RunPod account has ever been used with this code.** It is tested against a local mock of the RunPod API.
Treat the first real run as a supervised experiment with your own money at stake.

## Intended workflow (bring your own account; ELYREON runs nothing)
1. `echo "$KEY" | cinvar cloud connect` - validates the key with a read-only call and stores it (0600) in your
   config dir. Or set `CINVAR_RUNPOD_API_KEY`. Keys are never written to projects, jobs, exports or logs.
2. Enable: `security.allow_cloud_render = true` in `config.toml`, and start your MCP server with `CINVAR_MCP_PERMISSIONS=full`.
3. `cinvar.render.cloud {image, max_price_per_hr, max_runtime_minutes, ...}` returns a **quote** only.
4. **You** review and approve the quote in a terminal: `cinvar cloud approve <quote-id>` (shows image, price ceiling, runtime, worst case; type `APPROVE`). Approval is single-use, expires in 15 minutes, and cannot be created by an agent or plugin.
5. The agent then re-calls with `confirm_quote` and `acknowledge_billing_risk=true` (optionally `max_spend_usd`). Only then is a pod created.
6. CINVAR uploads the packaged project, polls, downloads and **verifies** the result, then terminates the pod.
   `cinvar.cloud.cleanup` terminates leftovers; `cinvar.jobs.retry` recovers interrupted jobs.

## Cost and billing limits (read this)
- RunPod provides **no provider-side hard spend cap** through this API. `max_price_per_hr`, `max_runtime_minutes`
  and `max_spend_usd` are **local best-effort checks**; the worker self-stops at its runtime limit, and a pod whose
  price exceeds your ceiling is terminated. A crash, network loss or bug can still leave a pod running and billing.
- Always check the RunPod console, set your own account spending limits, and use `cinvar.cloud.cleanup`.
- `deploy/runpod/Dockerfile` is an untested template.
- Local rendering is never restricted or degraded in favour of the cloud; RunPod is optional.

## Help wanted
A supervised live smoke test with a spend cap (see ROADMAP.md).

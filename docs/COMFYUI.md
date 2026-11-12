# ComfyUI (EXPERIMENTAL - mock-tested only)

CINVAR talks to **your existing ComfyUI** over HTTP (`CINVAR_COMFYUI_URL`, default `http://127.0.0.1:8188`). ComfyUI
(GPL-3.0) and its models are not bundled and CINVAR never downloads models. Live validation against a real ComfyUI has
**not** been done.

## Workflow descriptors
Drop JSON files in `<config dir>/comfy-workflows/`:

    { "name": "upscale-x2", "task": "upscale",
      "description": "2x image upscale",
      "workflow": { /* ComfyUI API-format graph (Save (API Format)) */ },
      "params": { "image": {"node": "10", "input": "image", "description": "source frame"} },
      "file_params": ["image"] }

`task` is one of: upscale, remove_background, enhance, inpaint, transition, vfx, restore, broll, other
(open contract for interpolation, tracking, audio enhancement etc. - use `other`).

## Tools
`cinvar.ai.workflows` (list + server reachability) -> `cinvar.ai.run_workflow {workflow, params, timeout_seconds}` ->
`cinvar.ai.import_outputs {job_id, place_at?}`. Before queuing, the graph is checked against the server's `/object_info`,
so a missing node type or model fails with a clear message. File parameters are uploaded first (`asset:<id>` or a
project-relative path). Jobs are cancellable (`/interrupt`).

## Safety
Only workflows you register are runnable; agents cannot submit arbitrary graphs. Sending frames to a *remote* ComfyUI
server sends your media off-device - keep it on localhost unless you mean it.

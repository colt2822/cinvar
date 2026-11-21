#!/usr/bin/env python3
"""Build the reviewer sample project: one healthy clip and one clip whose source file was deleted.

Usage: make_sample.py <cinvar-binary> <output-dir>
Result: <output-dir>/inspector-sample (a Cinvar project). Needs ffmpeg. Expected findings:
see EXPECTED.md next to this file.
"""
import json, os, shutil, subprocess, sys, tempfile

binary, out = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
os.makedirs(out, exist_ok=True)
work = tempfile.mkdtemp()
env = dict(os.environ, CINVAR_CONFIG_DIR=os.path.join(work, "cfg"), CINVAR_MEDIA_ROOTS=work)
subprocess.run([binary, "project", "create", "--name", "inspector-sample", "--dir", out],
               check=True, env=env, stdout=subprocess.DEVNULL)
project = os.path.join(out, "inspector-sample")

for name, freq in (("good", 440), ("lost", 880)):
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=640x360:rate=30:duration=2",
                    "-f", "lavfi", "-i", f"sine=frequency={freq}:duration=2", "-shortest",
                    "-pix_fmt", "yuv420p", os.path.join(work, f"{name}.mp4")], check=True)

srv = subprocess.Popen([binary, "mcp", "serve", "--project", project], stdin=subprocess.PIPE,
                       stdout=subprocess.PIPE, text=True, env=env)
n = 0
def call(tool, args=None):
    global n
    n += 1
    srv.stdin.write(json.dumps({"jsonrpc": "2.0", "id": n, "method": "tools/call",
                                "params": {"name": tool, "arguments": args or {}}}) + "\n")
    srv.stdin.flush()
    r = json.loads(srv.stdout.readline())["result"]
    body = json.loads(r["content"][0]["text"])
    if r.get("isError"):
        sys.exit(f"{tool} failed: {body}")
    return body

ids = {}
for name in ("good", "lost"):
    ids[name] = call("cinvar.media.import", {"path": os.path.join(work, f"{name}.mp4")})
    print(name, ids[name])
call("cinvar.timeline.create", {"name": "Main", "width": 1280, "height": 720})
asset = lambda b: b["asset_id"]
for name in ("good", "lost"):
    call("cinvar.timeline.append_clip", {"asset_id": asset(ids[name])})
srv.terminate()

# The known issue: delete the imported copy of "lost" so the clip points at a missing file.
removed = 0
lost_file = os.path.join(project, ids["lost"]["path"])
os.remove(lost_file); removed = 1
shutil.rmtree(os.path.join(project, "sessions"), ignore_errors=True)  # drop the build-time call log
print("removed", removed, "file(s); sample at", project)
shutil.rmtree(work, ignore_errors=True)

#!/usr/bin/env python3
"""Ascension demo driver: runs real CINVAR tools over MCP stdio against lightweight fixtures.
Usage: ascension_demo.py <cinvar-binary> <workdir>
Every step asserts; any failure exits non-zero. Steps labelled REAL use real FFmpeg/Blender/etc.;
nothing here talks to RunPod or any paid service (the cloud step is the free quote only)."""
import json, os, subprocess, sys, time

BIN, WORK = sys.argv[1], os.path.abspath(sys.argv[2])
os.makedirs(WORK, exist_ok=True)
CFG = os.path.join(WORK, "config")
os.makedirs(CFG, exist_ok=True)
open(os.path.join(CFG, "config.toml"), "w").write("[security]\nallow_cloud_render = true\n")  # quote only; no key exists
ENV = dict(os.environ, CINVAR_CONFIG_DIR=CFG)
# A placeholder key and an unreachable endpoint: the cloud steps here can never reach RunPod.
ENV["CINVAR_RUNPOD_API_KEY"] = "rpa_DEMO_PLACEHOLDER_not_a_real_key"
ENV["CINVAR_RUNPOD_BASE"] = "http://127.0.0.1:9"
# Agents are confined to the project by default; the human (this script) allows the footage folder.
ENV["CINVAR_MEDIA_ROOTS"] = os.path.join(WORK, "footage")
RESULTS = []

def step(name, kind, ok, detail=""):
    RESULTS.append((name, kind, ok, detail))
    print(("PASS " if ok else "FAIL ") + "[%s] %s %s" % (kind, name, detail), flush=True)
    if not ok:
        sys.exit(1)

def sh(*a, **k):
    return subprocess.run(a, env=ENV, capture_output=True, text=True, **k)

def probe(path):
    r = sh("ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", path)
    return r.stdout.strip()

class Session:
    def __init__(self, project):
        self.p = subprocess.Popen([BIN, "mcp", "serve", "--project", project], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=ENV)
        self.i = 0
        self.rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "demo", "version": "1"}})
    def rpc(self, m, params):
        self.i += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.i, "method": m, "params": params}) + "\n"); self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())
    def call(self, _tool, **args):
        r = self.rpc("tools/call", {"name": _tool, "arguments": args})
        if "error" in r:
            return {"_error": r["error"]}
        res = r["result"]
        try:
            body = json.loads(res["content"][0]["text"])
        except Exception:
            body = {"text": res["content"][0]["text"]}
        if res.get("isError"):
            body["_error"] = body.get("error", True)
        return body
    def kill(self):
        self.p.kill(); self.p.wait()
    def close(self):
        self.p.terminate(); self.p.wait()

# ---- fixtures -------------------------------------------------------------------------------
foot = os.path.join(WORK, "footage"); os.makedirs(foot, exist_ok=True)
for n, src in (("a.mp4", "testsrc2=size=640x360:rate=24:duration=6"), ("b.mp4", "smptebars=size=640x360:rate=24:duration=6")):
    sh("ffmpeg", "-y", "-v", "error", "-f", "lavfi", "-i", src, "-f", "lavfi", "-i", "sine=frequency=330:duration=6", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", os.path.join(foot, n))
r = sh(BIN, "project", "create", "--name", "Ascension", "--dir", WORK)
PROJECT = os.path.join(WORK, "ascension")
step("create project", "REAL", os.path.isdir(PROJECT))
s = Session(PROJECT)

# 1 import, 2 multitrack, 3 title, 4 captions, 5 audio
a = s.call("cinvar.media.import", path=os.path.join(foot, "a.mp4"))
b = s.call("cinvar.media.import", path=os.path.join(foot, "b.mp4"))
step("import media (content-validated)", "REAL", "asset_id" in a and "asset_id" in b)
step("hostile import refused", "REAL", "_error" in s.call("cinvar.media.import", path="/etc/passwd"))
tl = s.call("cinvar.timeline.create", name="Main", width=1920, height=1080, fps="24")
step("create timeline", "REAL", "_error" not in tl, str(list(tl)[:4]))
v2 = s.call("cinvar.timeline.add_track", kind="video", name="Overlay")
step("second video track (multitrack)", "REAL", "_error" not in v2)
c1 = s.call("cinvar.timeline.append_clip", asset_id=a["asset_id"])
c2 = s.call("cinvar.timeline.append_clip", asset_id=b["asset_id"])
step("two clips placed", "REAL", "_error" not in c1 and "_error" not in c2, str(c1)[:80])
clip1 = c1.get("clip_id") or c1.get("id")
title = s.call("cinvar.timeline.text", text="ASCENSION", at="0", duration="3", template="title", animate_in="fade")
step("animated title", "REAL", "_error" not in title, str(title)[:100])
cap = s.call("cinvar.captions.from_script", script="Welcome to the demo. Every step here really executes.", start="0", end="6")
step("captions from script", "REAL", "_error" not in cap and cap.get("cue_count", 0) >= 1, str(cap)[:100])
au = s.call("cinvar.timeline.clip_audio", clip_id=clip1, normalize=True, fade_out="1")
step("audio processing (normalize + fade)", "REAL", "_error" not in au, str(au)[:100])
sub = s.call("cinvar.captions.export", output="exports/demo.srt")
step("subtitle export", "REAL", os.path.isfile(os.path.join(PROJECT, "exports/demo.srt")))

# 6 real 4K, 7 8K
def export(out, w, h, wait=240):
    j = s.call("cinvar.render.export", output=out, width=w, height=h)
    if "_error" in j:
        return j
    jid = j.get("job_id")
    for _ in range(wait):
        st = s.call("cinvar.render.status", job_id=jid, wait_seconds=1)
        if st.get("status") in ("succeeded", "failed", "cancelled"):
            return st
    return {"_error": "timeout"}
st = export("renders/demo_4k.mp4", 3840, 2160)
step("4K export", "REAL", st.get("status") == "succeeded" and probe(os.path.join(PROJECT, "renders/demo_4k.mp4")) == "3840,2160", str(st.get("status")))
pf = s.call("cinvar.render.preflight", size_preset="8k")
step("8K preflight", "REAL", "_error" not in pf)
st = export("renders/demo_8k.mp4", 7680, 4320, 400)
p8 = os.path.join(PROJECT, "renders/demo_8k.mp4")
dec = sh("ffmpeg", "-v", "error", "-i", p8, "-f", "null", "-")
step("8K export decodes cleanly", "REAL", st.get("status") == "succeeded" and probe(p8) == "7680,4320" and dec.returncode == 0 and dec.stderr.strip() == "", str(st.get("status")))
step("duplicate export refused", "REAL", "duplicate_of" in s.call("cinvar.render.export", output="renders/demo_4k.mp4", width=3840, height=2160))

# 9 OTIO round trip
ex = s.call("cinvar.otio.export", output="exports/demo.otio")
step("OTIO export", "REAL", "_error" not in ex and os.path.isfile(os.path.join(PROJECT, "exports/demo.otio")))
im = s.call("cinvar.otio.import", input=os.path.join(PROJECT, "exports/demo.otio"))
step("OTIO import", "REAL", "_error" not in im, str(im)[:100])

# 10 cloud packaging (free quote only)
q = s.call("cinvar.render.cloud", max_price_per_hr=1.0, max_runtime_minutes=30, image="ghcr.io/example/cinvar-worker:demo", size_preset="4k")
step("RunPod job packaged + quoted (nothing created, nothing billed)", "REAL-LOCAL", q.get("billed") is False and "quote" in q, "worst_case=%s upload=%s" % (q.get("quote", {}).get("worst_case_usd"), q.get("quote", {}).get("upload_bytes_total")))
step("billing without acknowledgement/key refused", "REAL-LOCAL", "_error" in s.call("cinvar.render.cloud", max_price_per_hr=1.0, max_runtime_minutes=30, image="x", confirm_quote=q.get("quote", {}).get("id", "none")))

# 13 MCP inspection
tools = s.rpc("tools/list", {})["result"]["tools"]
step("MCP tool inspection", "REAL", len(tools) > 70, "%d tools" % len(tools))
s.close()

# 14 cancellation, 15 recovery
s = Session(PROJECT)
j = s.call("cinvar.render.export", output="renders/cancel_me.mp4", width=7680, height=4320, crf=30)
time.sleep(1.0)
c = s.call("cinvar.render.cancel", job_id=j.get("job_id"))
t0 = time.time()
st = s.call("cinvar.render.status", job_id=j.get("job_id"), wait_seconds=60)
step("cancellation", "REAL", st.get("status") == "cancelled", "%s after %.1fs" % (st.get("status"), time.time() - t0))
j = s.call("cinvar.render.export", output="renders/recover_me.mp4", width=3840, height=2160, crf=30)
time.sleep(0.8)
s.kill()  # simulated crash: SIGKILL mid-render
s = Session(PROJECT)
lst = s.call("cinvar.jobs.list")
jobs = lst.get("jobs", [])
mine = [x for x in jobs if "recover_me" in json.dumps(x)]
step("crashed job recorded as interrupted (not restarted)", "REAL", bool(mine) and mine[0].get("state") == "interrupted", str(mine[0].get("state") if mine else None))
rt = s.call("cinvar.jobs.retry", job_id=mine[0]["id"])
step("interrupted job retried", "REAL", "_error" not in rt, str(rt)[:100])
# retry re-queues the durable record; re-issuing the identical export adopts it and runs it
again = s.call("cinvar.render.export", output="renders/recover_me.mp4", width=3840, height=2160, crf=30)
st = s.call("cinvar.render.status", job_id=again.get("job_id"), wait_seconds=240)
step("recovered job finished", "REAL", "duplicate_of" not in again and st.get("status") == "succeeded" and probe(os.path.join(PROJECT, "renders/recover_me.mp4")) == "3840,2160", str(st.get("status")))
s.close()

# Optional: OCIO if configured
if os.environ.get("CINVAR_OCIO_PYTHON"):
    s = Session(PROJECT)
    info = s.call("cinvar.color.inspect")
    step("OpenColorIO config discovery", "REAL", "color_spaces" in info, info.get("config_source", ""))
    s.close()
else:
    print("SKIP [REAL] OpenColorIO (set CINVAR_OCIO_PYTHON)")

print("\nDEMO OK: %d steps passed" % len(RESULTS))
print("PROJECT=" + PROJECT)

#!/usr/bin/env python3
"""Demo 2, 3 and 4: revisions and vertical export driven from an external MCP client.

Every assertion is made against what the CINVAR MCP server actually returned.
Run:  python3 examples/verify_mcp.py <project-dir> [binary]
"""
import json
import os
import subprocess
import sys


class Mcp:
    def __init__(self, binary, project):
        self.proc = subprocess.Popen(
            [binary, "mcp", "serve", "--project", project],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.next_id = 1

    def request(self, method, params=None):
        rid = self.next_id
        self.next_id += 1
        payload = {"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("server closed the connection")
            message = json.loads(line)
            if message.get("id") == rid:
                return message

    def call(self, name, arguments):
        message = self.request("tools/call", {"name": name, "arguments": arguments})
        if "error" in message:
            raise RuntimeError("jsonrpc error: " + json.dumps(message["error"]))
        result = message["result"]
        text = result["content"][0]["text"]
        try:
            payload = json.loads(text)
        except ValueError:
            payload = text
        return result.get("isError", False), payload

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=30)


def check(label, condition, detail):
    print(("PASS  " if condition else "FAIL  ") + label + ": " + str(detail))
    return condition


def clip_list(project_payload):
    for timeline in project_payload["project"]["timelines"]:
        if timeline["id"] == project_payload["project"].get("active_timeline"):
            return timeline
    return project_payload["project"]["timelines"][0]


def video_clips(timeline):
    clips = []
    for track in timeline["tracks"]:
        if track["kind"] == "video":
            clips.extend(track["clips"])
    return sorted(clips, key=lambda c: c["start"])


def probe_dims(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0",
         "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x", path],
        capture_output=True, text=True, check=True,
    )
    return out.stdout.strip()


def main():
    project = sys.argv[1]
    binary = sys.argv[2] if len(sys.argv) > 2 else "target/debug/cinvar"
    client = Mcp(binary, project)
    ok = True
    try:
        init = client.request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}})
        ok &= check("mcp.initialize", init["result"]["serverInfo"]["name"] == "cinvar",
                    init["result"]["serverInfo"])

        listed = client.request("tools/list")
        names = [tool["name"] for tool in listed["result"]["tools"]]
        ok &= check("mcp.tools.list", len(names) >= 40, str(len(names)) + " tools")

        error, history = client.call("cinvar.project.history", {})
        revisions = history.get("revisions", [])
        ok &= check("history.before_edit", len(revisions) >= 2, str(len(revisions)) + " revisions")
        keep_revision = revisions[0]["revision"]

        error, status = client.call("cinvar.project.status", {})
        timeline = clip_list(status)
        before = video_clips(timeline)
        ok &= check("revision.before", len(before) == 3, [c["name"] for c in before])
        duration_before = timeline["duration"]

        target = before[1]["id"]
        error, removal = client.call("cinvar.timeline.remove", {"clip_id": target, "ripple": True})
        ok &= check("revision.remove", (not error) and removal.get("ok") is True, removal)

        error, status = client.call("cinvar.project.status", {})
        timeline = clip_list(status)
        after = video_clips(timeline)
        ok &= check("revision.clip_count", len(after) == 2, [c["name"] for c in after])
        ok &= check("revision.duration_shrank", timeline["duration"] < duration_before,
                    str(round(duration_before, 2)) + "s -> " + str(round(timeline["duration"], 2)) + "s")

        render_path = os.path.join(project, "renders", "revision.mp4")
        error, export = client.call("cinvar.render.export", {"output": render_path})
        job_id = export.get("job_id")
        ok &= check("render.export", (not error) and bool(job_id), export)

        error, snapshot = client.call("cinvar.render.status", {"job_id": job_id, "wait_seconds": 180})
        ok &= check("render.status", snapshot.get("status") == "succeeded",
                    snapshot.get("status") + " progress=" + str(snapshot.get("progress")))

        error, qa = client.call("cinvar.qa.render_check", {"path": render_path})
        ok &= check("qa.render_check", (not error) and qa.get("passed") is True,
                    "score=" + str(qa.get("score")))
        ok &= check("render.file_exists", os.path.getsize(render_path) > 1024,
                    str(os.path.getsize(render_path)) + " bytes")

        error, history = client.call("cinvar.project.history", {})
        revisions = history.get("revisions", [])
        ok &= check("history.recorded", len(revisions) >= 3, str(len(revisions)) + " revisions")

        error, rollback = client.call("cinvar.project.rollback", {"revision": keep_revision})
        ok &= check("revision.rollback", not error, "restored revision " + str(keep_revision))

        error, status = client.call("cinvar.project.status", {})
        restored = video_clips(clip_list(status))
        ok &= check("revision.rollback_restores_clips", len(restored) >= 3,
                    [c["name"] for c in restored])

        error, created = client.call("cinvar.timeline.create",
                                     {"name": "Vertical", "width": 1080, "height": 1920, "fps": "30"})
        ok &= check("vertical.timeline", (not error) and created.get("width") == 1080, created)
        asset_id = restored[0]["asset_id"]
        error, appended = client.call("cinvar.timeline.append_clip", {"asset_id": asset_id})
        ok &= check("vertical.append", not error, appended)
        error, captions = client.call("cinvar.timeline.captions", {"cues": [
            {"start": "0", "end": "1.5", "text": "READABLE CAPTION ONE"},
            {"start": "1.5", "end": "3", "text": "READABLE CAPTION TWO"},
        ], "size": 56, "position": "bottom"})
        ok &= check("vertical.captions", (not error) and bool(captions.get("caption_track_id")), captions)

        vertical_path = os.path.join(project, "renders", "vertical.mp4")
        error, export = client.call("cinvar.render.export", {"output": vertical_path})
        error, snapshot = client.call("cinvar.render.status",
                                     {"job_id": export.get("job_id"), "wait_seconds": 180})
        ok &= check("vertical.render", snapshot.get("status") == "succeeded", snapshot.get("status"))
        dims = probe_dims(vertical_path)
        ok &= check("vertical.dimensions", dims == "1080x1920", dims)
        error, qa = client.call("cinvar.qa.render_check", {"path": vertical_path})
        ok &= check("vertical.qa", (not error) and qa.get("passed") is True, "score=" + str(qa.get("score")))

        error, blender = client.call("cinvar.blender.status", {})
        print("INFO  blender.status: " + json.dumps(blender))
    finally:
        client.close()
    print("RESULT " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

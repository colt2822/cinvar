#!/usr/bin/env python3
"""CINVAR persistent headless Blender worker (Argon 5E) -- the in-Blender server side.

This file is embedded in the cinvar binary (include_str!) and written to
"<cache>/cinvar/blender-worker/worker_server.py" at spawn time, then handed to Blender:

    blender -b --factory-startup --python worker_server.py

It serves the unix domain socket "~/.cache/cinvar/blender-worker.sock" (overridable with
CINVAR_WORKER_SOCKET).  Wire format, which must stay byte-identical with src/blender/worker.rs:

    request  := u32_be length || length bytes of UTF-8 JSON
    response := u32_be length || length bytes of UTF-8 JSON

One connection carries exactly one request and one response, then closes.  The explicit
length prefix is what makes a short read impossible to mistake for a complete message:
both ends read exactly "length" bytes or fail loudly.

Job isolation: every request may carry "job".  When the job id differs from the one the
worker last saw, the scene is factory-reset and verified empty *before* the operation runs,
so state can never leak from one job into the next.  Reset is therefore explicit and
verified rather than implicit and hoped-for.

Exit codes: 0 = clean shutdown, 3 = another worker already owns the socket.
"""

import json
import os
import socket
import struct
import sys
import time
import traceback

HEADER = 4
MAX_FRAME = 8 * 1024 * 1024
PROTOCOL = 1
DEFAULT_SOCKET = os.path.join(
    os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache"),
    "cinvar",
    "blender-worker.sock",
)
IDLE_SECS = float(os.environ.get("CINVAR_WORKER_IDLE_SECS") or 1800.0)

HERE = os.path.dirname(os.path.abspath(__file__))


def log(message):
    sys.stdout.write("CINVAR_WORKER %s\n" % message)
    sys.stdout.flush()


class WorkerError(Exception):
    """A request-level failure reported to the caller, never fatal to the worker."""

    def __init__(self, code, message, hint=None):
        Exception.__init__(self, message)
        self.code = code
        self.message = message
        self.hint = hint


class ProtocolError(Exception):
    pass


# --------------------------------------------------------------------------------------
# framing
# --------------------------------------------------------------------------------------


def _read_exact(conn, count):
    chunks = []
    got = 0
    while got < count:
        chunk = conn.recv(count - got)
        if not chunk:
            raise ProtocolError("peer closed after %d of %d bytes" % (got, count))
        chunks.append(chunk)
        got += len(chunk)
    return b"".join(chunks)


def read_frame(conn):
    (length,) = struct.unpack(">I", _read_exact(conn, HEADER))
    if length == 0 or length > MAX_FRAME:
        raise ProtocolError("refusing frame of %d bytes" % length)
    return json.loads(_read_exact(conn, length).decode("utf-8"))


def write_frame(conn, payload):
    body = json.dumps(payload, separators=(",", ":"), default=str).encode("utf-8")
    if len(body) > MAX_FRAME:
        raise ProtocolError("response of %d bytes exceeds the frame cap" % len(body))
    conn.sendall(struct.pack(">I", len(body)) + body)


# --------------------------------------------------------------------------------------
# scene operations (each handler returns JSON-able data, or raises WorkerError)
# --------------------------------------------------------------------------------------


def _bpy():
    import bpy  # imported lazily so this module can be linted outside Blender

    return bpy


def _scene():
    return _bpy().context.scene


def scene_state():
    bpy = _bpy()
    scene = _scene()
    names = [obj.name for obj in scene.objects]
    camera = scene.camera
    return {
        "scene": scene.name,
        "objects": names,
        "object_count": len(names),
        "frame": int(scene.frame_current),
        "engine": scene.render.engine,
        "resolution": [int(scene.render.resolution_x), int(scene.render.resolution_y)],
        "camera": camera.name if camera is not None else None,
        "blend": bpy.data.filepath or None,
        "dirty": bool(bpy.data.is_dirty),
    }


def load_sdk(state):
    sdk = state.get("sdk")
    if sdk is not None:
        return sdk
    if HERE not in sys.path:
        sys.path.insert(0, HERE)
    import cinvar_bpy_sdk

    state["sdk"] = cinvar_bpy_sdk
    return cinvar_bpy_sdk


def op_ping(state, params):
    bpy = _bpy()
    return {
        "pong": True,
        "pid": os.getpid(),
        "protocol": PROTOCOL,
        "blender": bpy.app.version_string,
        "job": state.get("job"),
        "gui_attached": bool(state.get("gui_attached")),
        "uptime_s": round(time.time() - state["started"], 3),
        "requests": state["requests"],
        "scene": scene_state(),
    }


def op_version(state, params):
    bpy = _bpy()
    return {
        "protocol": PROTOCOL,
        "blender": bpy.app.version_string,
        "blender_version": list(bpy.app.version),
        "python": sys.version.split()[0],
        "platform": sys.platform,
        "executable": bpy.app.binary_path,
        "background": bool(bpy.app.background),
        "socket": state.get("socket"),
        "pid": os.getpid(),
        "sdk": os.path.join(HERE, "cinvar_bpy_sdk.py"),
    }


def op_reset(state, params):
    """Factory reset to an empty scene, then *verify* the reset actually emptied it."""
    bpy = _bpy()
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = _scene()
    names = [obj.name for obj in scene.objects]
    if names:
        raise WorkerError(
            "internal_error",
            "scene reset was not clean: %d object(s) survived (%s)"
            % (len(names), ", ".join(names[:8])),
        )
    return {
        "verified": True,
        "reset": "factory_empty",
        "orphan_meshes": len(bpy.data.meshes),
        **scene_state(),
    }


def op_load(state, params):
    bpy = _bpy()
    path = params.get("path")
    if not path:
        raise WorkerError("invalid_input", "load requires params.path")
    target = os.path.realpath(os.path.abspath(os.path.expanduser(str(path))))
    if not os.path.isfile(target):
        raise WorkerError("not_found", "no such .blend file: %s" % target)
    expect = params.get("sha256")
    if expect:
        import hashlib

        digest = hashlib.sha256()
        with open(target, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
        actual = digest.hexdigest()
        if actual != str(expect):
            raise WorkerError(
                "conflict",
                "checkpoint digest mismatch: expected %s, found %s" % (expect, actual),
                hint="the checkpoint changed after it was cached; re-save it",
            )
    if params.get("reset_first", False):
        op_reset(state, {})
    started = time.time()
    try:
        bpy.ops.wm.open_mainfile(filepath=target)
    except Exception as exc:  # noqa: BLE001 - surfaced to the caller verbatim
        raise WorkerError("render_failed", "open_mainfile failed: %s" % exc)
    state["loaded"] = target
    result = scene_state()
    result["path"] = target
    result["load_ms"] = round((time.time() - started) * 1000.0, 3)
    expect_count = params.get("expect_objects")
    if expect_count is not None and int(expect_count) != result["object_count"]:
        raise WorkerError(
            "conflict",
            "loaded scene has %d object(s), expected %d"
            % (result["object_count"], int(expect_count)),
        )
    return result


def op_save(state, params):
    """Write the current scene as a reusable checkpoint (a cached scene)."""
    bpy = _bpy()
    path = params.get("path")
    if not path:
        raise WorkerError("invalid_input", "save requires params.path")
    path = os.path.abspath(os.path.expanduser(str(path)))
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    before = scene_state()
    try:
        bpy.ops.wm.save_as_mainfile(filepath=path, copy=True, compress=True)
    except Exception as exc:  # noqa: BLE001
        raise WorkerError("io_error", "save_as_mainfile failed: %s" % exc)
    if not os.path.isfile(path):
        raise WorkerError("io_error", "checkpoint was not written: %s" % path)
    import hashlib

    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return {
        "path": path,
        "bytes": os.path.getsize(path),
        "sha256": digest.hexdigest(),
        "object_count": before["object_count"],
        "objects": before["objects"],
    }


def op_objects(state, params):
    return scene_state()


def op_measure(state, params):
    """Run the measurement SDK against the live scene (no process startup)."""
    sdk = load_sdk(state)
    out_dir = params.get("out_dir")
    if not out_dir:
        raise WorkerError("invalid_input", "measure requires params.out_dir")
    out_dir = os.path.abspath(os.path.expanduser(str(out_dir)))
    os.makedirs(out_dir, exist_ok=True)
    scene = _scene()
    frame = sdk.scene_frame_choice(scene, params.get("frame", "middle"))
    floor_z = float(params.get("floor_z", 0.0))
    started = time.time()
    diagnostics = sdk.collect(scene, frame, floor_z)
    if params.get("render_views", False):
        flagged = set()
        for report in diagnostics.get("objects", []):
            if report.get("outside_pct", 0.0) > 0.0:
                flagged.add(report["name"])
        size = int(params.get("size", 384))
        try:
            diagnostics["views"] = sdk.render_views(scene, out_dir, size, flagged)
            camera_view = diagnostics["views"].get("camera")
            diagnostics["image"] = (
                sdk.image_stats(camera_view) if camera_view else sdk.image_stats("")
            )
        except Exception as exc:  # noqa: BLE001 - a view render must not lose measurements
            diagnostics["views_error"] = str(exc)
    target = os.path.join(out_dir, "diagnostics.json")
    sdk.write_json(target, diagnostics)
    objects = diagnostics.get("objects", []) or []
    return {
        "diagnostics": target,
        "frame": int(diagnostics.get("frame", frame)),
        "objects": len(objects),
        "object_names": [report.get("name") for report in objects],
        "intersections": len(diagnostics.get("intersections", []) or []),
        "scene": diagnostics.get("scene"),
        "camera": diagnostics.get("camera"),
        "views": sorted(diagnostics.get("views", {}).keys()),
        "ms": round((time.time() - started) * 1000.0, 3),
    }


def op_render_frames(state, params):
    bpy = _bpy()
    out_dir = params.get("out_dir")
    if not out_dir:
        raise WorkerError("invalid_input", "render_frames requires params.out_dir")
    out_dir = os.path.abspath(os.path.expanduser(str(out_dir)))
    os.makedirs(out_dir, exist_ok=True)
    scene = _scene()
    start = int(params.get("start", scene.frame_start))
    end = int(params.get("end", scene.frame_end))
    if end < start:
        raise WorkerError("invalid_input", "end %d is before start %d" % (end, start))
    if end - start + 1 > 10000:
        raise WorkerError("invalid_input", "refusing %d frames in one request" % (end - start + 1))
    if params.get("width"):
        scene.render.resolution_x = int(params["width"])
    if params.get("height"):
        scene.render.resolution_y = int(params["height"])
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA" if params.get("transparent", False) else "RGB"
    scene.render.film_transparent = bool(params.get("transparent", False))
    engine = params.get("engine")
    if engine:
        try:
            scene.render.engine = str(engine)
        except Exception as exc:  # noqa: BLE001
            raise WorkerError("invalid_input", "unknown render engine %r: %s" % (engine, exc))
    started = time.time()
    written = []
    for frame in range(start, end + 1):
        scene.frame_set(frame)
        target = os.path.join(out_dir, "frame_%04d.png" % frame)
        scene.render.filepath = target
        try:
            bpy.ops.render.render(write_still=True)
        except Exception as exc:  # noqa: BLE001
            raise WorkerError("render_failed", "frame %d failed: %s" % (frame, exc))
        if not os.path.isfile(target) or os.path.getsize(target) == 0:
            raise WorkerError("render_failed", "frame %d produced no PNG at %s" % (frame, target))
        written.append(target)
    return {
        "frames": len(written),
        "dir": out_dir,
        "start": start,
        "end": end,
        "engine": scene.render.engine,
        "ms": round((time.time() - started) * 1000.0, 3),
    }


def op_render_preview(state, params):
    sdk = load_sdk(state)
    out_dir = params.get("out_dir")
    if not out_dir:
        raise WorkerError("invalid_input", "render_preview requires params.out_dir")
    out_dir = os.path.abspath(os.path.expanduser(str(out_dir)))
    os.makedirs(out_dir, exist_ok=True)
    scene = _scene()
    frame = sdk.scene_frame_choice(scene, params.get("frame", "middle"))
    started = time.time()
    views = sdk.render_views(scene, out_dir, int(params.get("size", 512)))
    return {
        "views": views,
        "frame": int(frame),
        "dir": out_dir,
        "ms": round((time.time() - started) * 1000.0, 3),
    }


def op_attach(state, params):
    """Record that a caller attached this worker to an authorized GUI session.

    Authorization itself is enforced by the Rust client (an operator opt-in plus an
    explicit flag from the caller) and re-checked here: the worker never accepts an
    unattributed attach.
    """
    if params.get("authorized") is not True:
        raise WorkerError(
            "permission_denied",
            "GUI attachment is not authorized for this request",
            hint="set CINVAR_GUI_ATTACH=1 and pass authorized=true",
        )
    state["gui_attached"] = True
    state["gui_socket"] = params.get("gui_socket")
    return {
        "attached": True,
        "gui_socket": state.get("gui_socket"),
        "headless_unaffected": True,
        "scene": scene_state(),
    }


def op_shutdown(state, params):
    return {"stopping": True, "pid": os.getpid(), "requests": state["requests"]}


HANDLERS = {
    "ping": op_ping,
    "version": op_version,
    "reset": op_reset,
    "load": op_load,
    "save": op_save,
    "objects": op_objects,
    "measure": op_measure,
    "render_frames": op_render_frames,
    "render_preview": op_render_preview,
    "attach": op_attach,
    "shutdown": op_shutdown,
}

# Ops that touch the scene, and therefore require the job boundary to be clean first.
SCENE_OPS = frozenset(
    ["reset", "load", "save", "objects", "measure", "render_frames", "render_preview"]
)


def dispatch(request, state):
    if not isinstance(request, dict):
        raise WorkerError("protocol_error", "request must be a JSON object")
    op = str(request.get("op") or "")
    handler = HANDLERS.get(op)
    if handler is None:
        raise WorkerError(
            "unsupported",
            "unknown operation %r" % op,
            hint="supported: %s" % ", ".join(sorted(HANDLERS)),
        )
    params = request.get("params") or {}
    if not isinstance(params, dict):
        raise WorkerError("protocol_error", "params must be a JSON object")
    job = request.get("job")
    auto_reset = False
    previous = state.get("job")
    if job is not None and op in SCENE_OPS and str(job) != previous:
        op_reset(state, {})
        state["job"] = str(job)
        auto_reset = True
    result = handler(state, params)
    if auto_reset:
        result["auto_reset"] = True
        result["previous_job"] = previous
    return result


# --------------------------------------------------------------------------------------
# server loop
# --------------------------------------------------------------------------------------


def prepare_socket(socket_path):
    directory = os.path.dirname(socket_path) or "."
    os.makedirs(directory, exist_ok=True)
    if os.path.exists(socket_path):
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        probe.settimeout(1.0)
        try:
            probe.connect(socket_path)
        except OSError:
            os.unlink(socket_path)
            log("reaped stale socket %s" % socket_path)
        else:
            log("another worker already listens on %s" % socket_path)
            raise SystemExit(3)
        finally:
            probe.close()


def serve(socket_path):
    _bpy()  # fail fast (and loudly) if we are not inside Blender
    prepare_socket(socket_path)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(socket_path)
    os.chmod(socket_path, 0o600)
    listener.listen(8)
    listener.settimeout(1.0)
    state = {
        "started": time.time(),
        "requests": 0,
        "job": None,
        "socket": socket_path,
    }
    log(
        "listening on %s (pid %d, blender %s, protocol %d)"
        % (socket_path, os.getpid(), _bpy().app.version_string, PROTOCOL)
    )
    last_seen = time.time()
    stopping = False
    while not stopping:
        try:
            conn, _ = listener.accept()
        except socket.timeout:
            if IDLE_SECS > 0 and time.time() - last_seen > IDLE_SECS:
                log("idle for %ss, exiting" % int(IDLE_SECS))
                break
            continue
        except KeyboardInterrupt:
            break
        last_seen = time.time()
        conn.settimeout(None)
        request_id = 0
        op = ""
        started = time.time()
        try:
            request = read_frame(conn)
            if isinstance(request, dict):
                request_id = int(request.get("id") or 0)
                op = str(request.get("op") or "")
            state["requests"] += 1
            result = dispatch(request, state)
            response = {"id": request_id, "ok": True, "result": result}
            if op == "shutdown":
                stopping = True
        except WorkerError as exc:
            response = {
                "id": request_id,
                "ok": False,
                "error": {"code": exc.code, "message": exc.message, "hint": exc.hint},
            }
            log("op %r failed: %s: %s" % (op, exc.code, exc.message))
        except ProtocolError as exc:
            log("protocol error: %s" % exc)
            try:
                write_frame(
                    conn,
                    {
                        "id": request_id,
                        "ok": False,
                        "error": {"code": "protocol_error", "message": str(exc)},
                    },
                )
            except Exception:  # noqa: BLE001 - the peer is gone; nothing to report to
                pass
            conn.close()
            continue
        except Exception as exc:  # noqa: BLE001 - a bad op must never kill the worker
            detail = traceback.format_exc()
            log("op %r crashed:\n%s" % (op, detail))
            response = {
                "id": request_id,
                "ok": False,
                "error": {
                    "code": "internal_error",
                    "message": "%s: %s" % (type(exc).__name__, exc),
                    "hint": detail.strip().splitlines()[-1] if detail.strip() else None,
                },
            }
        response["ms"] = round((time.time() - started) * 1000.0, 3)
        response["op"] = op
        try:
            write_frame(conn, response)
        except Exception as exc:  # noqa: BLE001
            log("could not answer %r: %s" % (op, exc))
        finally:
            conn.close()
    try:
        listener.close()
    finally:
        if os.path.exists(socket_path):
            try:
                os.unlink(socket_path)
            except OSError:
                pass
    log("worker stopped")


def main():
    socket_path = os.environ.get("CINVAR_WORKER_SOCKET") or DEFAULT_SOCKET
    log("booting worker for %s" % socket_path)
    serve(socket_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())

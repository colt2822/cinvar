#!/usr/bin/env python3
"""CINVAR GUI attach bridge (Argon 5E, optional) -- the GUI-side half of the attach path.

Headless operation never needs this file.  It exists so an explicitly authorized,
interactive Blender session can be attached to: the user watches the scene being built,
moves or locks the camera, and rendering then continues from the approved scene.

Two ways to load it into a GUI Blender session:

    blender --python worker_gui_addon.py            # ad-hoc
    (or install this file as an addon and enable "CINVAR GUI Bridge")

It listens on CINVAR_GUI_SOCKET (default ~/.cache/cinvar/blender-gui.sock) with the same
length-prefixed JSON framing as the headless worker:

    u32_be length || length bytes of UTF-8 JSON,  one request -> one response, then close.

Blender is not thread safe, so the socket thread only parses requests and hands them to a
queue that a bpy.app.timers callback drains on the main thread.  Supported ops: ping,
version, objects, load, save, measure, render_still, shutdown(bridge only).

Authorization is enforced on the CINVAR side (Rust): attach requires CINVAR_GUI_ATTACH=1,
an explicit authorized flag from the caller, and this bridge answering a ping.
"""

import json
import os
import queue
import socket
import struct
import sys
import threading
import time
import traceback

bl_info = {
    "name": "CINVAR GUI Bridge",
    "author": "CINVAR",
    "version": (0, 2, 0),
    "blender": (3, 6, 0),
    "location": "Background",
    "description": "Attaches an authorized GUI Blender session to the CINVAR worker",
    "category": "System",
}

HEADER = 4
MAX_FRAME = 8 * 1024 * 1024
PROTOCOL = 1

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = queue.Queue()
STATE = {"started": time.time(), "requests": 0, "stopping": False, "thread": None, "listener": None}


def default_socket():
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    return os.environ.get("CINVAR_GUI_SOCKET") or os.path.join(base, "cinvar", "blender-gui.sock")


def _read_exact(conn, count):
    chunks = []
    got = 0
    while got < count:
        chunk = conn.recv(count - got)
        if not chunk:
            raise OSError("peer closed after %d of %d bytes" % (got, count))
        chunks.append(chunk)
        got += len(chunk)
    return b"".join(chunks)


def read_frame(conn):
    (length,) = struct.unpack(">I", _read_exact(conn, HEADER))
    if length == 0 or length > MAX_FRAME:
        raise OSError("refusing frame of %d bytes" % length)
    return json.loads(_read_exact(conn, length).decode("utf-8"))


def write_frame(conn, payload):
    body = json.dumps(payload, separators=(",", ":"), default=str).encode("utf-8")
    conn.sendall(struct.pack(">I", len(body)) + body)


# ---------------------------------------------------------------- main-thread operations


def op_ping(state, params):
    import bpy

    return {
        "pong": True,
        "bridge": "gui",
        "protocol": PROTOCOL,
        "pid": os.getpid(),
        "blender": bpy.app.version_string,
        "background": bool(bpy.app.background),
        "requests": state["requests"],
        "scene": scene_state(),
    }


def scene_state():
    import bpy

    scene = bpy.context.scene
    names = [obj.name for obj in scene.objects]
    camera = scene.camera
    return {
        "scene": scene.name,
        "objects": names,
        "object_count": len(names),
        "frame": int(scene.frame_current),
        "camera": camera.name if camera is not None else None,
        "camera_location": list(camera.location) if camera is not None else None,
        "blend": bpy.data.filepath or None,
    }


def op_version(state, params):
    import bpy

    return {
        "protocol": PROTOCOL,
        "bridge": "gui",
        "blender": bpy.app.version_string,
        "background": bool(bpy.app.background),
        "socket": state.get("socket"),
    }


def op_objects(state, params):
    return scene_state()


def op_load(state, params):
    import bpy

    path = params.get("path")
    if not path:
        raise ValueError("load requires params.path")
    path = os.path.realpath(os.path.abspath(os.path.expanduser(str(path))))
    if not os.path.isfile(path):
        raise ValueError("no such .blend file: %s" % path)
    bpy.ops.wm.open_mainfile(filepath=path)
    return scene_state()


def op_save(state, params):
    import bpy

    path = params.get("path")
    if not path:
        raise ValueError("save requires params.path")
    path = os.path.abspath(os.path.expanduser(str(path)))
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    bpy.ops.wm.save_as_mainfile(filepath=path, copy=True)
    return {"path": path, "bytes": os.path.getsize(path), "objects": scene_state()["objects"]}


def op_measure(state, params):
    if HERE not in sys.path:
        sys.path.insert(0, HERE)
    import cinvar_bpy_sdk as sdk

    out_dir = params.get("out_dir") or os.path.join(os.path.expanduser("~"), ".cache", "cinvar", "gui-measure")
    os.makedirs(out_dir, exist_ok=True)
    import bpy

    scene = bpy.context.scene
    frame = sdk.scene_frame_choice(scene, params.get("frame", "middle"))
    diagnostics = sdk.collect(scene, frame, float(params.get("floor_z", 0.0)))
    target = os.path.join(out_dir, "diagnostics.json")
    sdk.write_json(target, diagnostics)
    return {
        "diagnostics": target,
        "frame": int(diagnostics.get("frame", frame)),
        "objects": len(diagnostics.get("objects", []) or []),
    }


def op_render_still(state, params):
    import bpy

    out = params.get("path")
    if not out:
        raise ValueError("render_still requires params.path")
    scene = bpy.context.scene
    if params.get("frame") is not None:
        scene.frame_set(int(params["frame"]))
    scene.render.filepath = os.path.abspath(os.path.expanduser(str(out)))
    bpy.ops.render.render(write_still=True)
    return {"path": scene.render.filepath, "bytes": os.path.getsize(scene.render.filepath)}


def op_shutdown(state, params):
    state["stopping"] = True
    return {"stopping": True, "bridge": "gui"}


HANDLERS = {
    "ping": op_ping,
    "version": op_version,
    "objects": op_objects,
    "load": op_load,
    "save": op_save,
    "measure": op_measure,
    "render_still": op_render_still,
    "shutdown": op_shutdown,
}


def handle(item):
    request, conn, _state = item
    request_id = 0
    op = ""
    started = time.time()
    try:
        request_id = int(request.get("id") or 0)
        op = str(request.get("op") or "")
        handler = HANDLERS.get(op)
        if handler is None:
            raise ValueError("unknown operation %r" % op)
        _state["requests"] += 1
        response = {"id": request_id, "ok": True, "result": handler(_state, request.get("params") or {})}
    except Exception as exc:  # noqa: BLE001 - report, never die
        response = {
            "id": request_id,
            "ok": False,
            "error": {
                "code": "internal_error",
                "message": "%s: %s" % (type(exc).__name__, exc),
                "hint": traceback.format_exc().strip().splitlines()[-1],
            },
        }
    response["op"] = op
    response["ms"] = round((time.time() - started) * 1000.0, 3)
    try:
        write_frame(conn, response)
    except Exception:  # noqa: BLE001
        pass
    finally:
        conn.close()


def _timer():
    """Main-thread drain: everything touching bpy runs here, never on the socket thread."""
    while True:
        try:
            item = WORK.get_nowait()
        except queue.Empty:
            break
        handle(item)
    return 0.05


def _serve(socket_path):
    directory = os.path.dirname(socket_path) or "."
    os.makedirs(directory, exist_ok=True)
    if os.path.exists(socket_path):
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        probe.settimeout(1.0)
        try:
            probe.connect(socket_path)
        except OSError:
            os.unlink(socket_path)
        else:
            print("CINVAR_GUI another bridge already listens on %s" % socket_path)
            raise SystemExit(3)
        finally:
            probe.close()
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(socket_path)
    os.chmod(socket_path, 0o600)
    listener.listen(4)
    STATE["listener"] = listener
    print("CINVAR_GUI listening on %s (pid %d)" % (socket_path, os.getpid()))
    while not STATE["stopping"]:
        try:
            conn, _ = listener.accept()
        except OSError:
            break
        try:
            request = read_frame(conn)
        except Exception as exc:  # noqa: BLE001
            print("CINVAR_GUI bad frame: %s" % exc)
            conn.close()
            continue
        WORK.put((request, conn, STATE))
    try:
        listener.close()
    finally:
        if os.path.exists(socket_path):
            try:
                os.unlink(socket_path)
            except OSError:
                pass


def start(socket_path=None):
    import bpy

    if STATE["thread"] is not None:
        return STATE["thread"]
    socket_path = socket_path or default_socket()
    STATE["socket"] = socket_path
    thread = threading.Thread(target=_serve, args=(socket_path,), name="cinvar-gui-bridge", daemon=True)
    thread.start()
    STATE["thread"] = thread
    bpy.app.timers.register(_timer, first_interval=0.1, persistent=True)
    return thread


def stop():
    import bpy

    STATE["stopping"] = True
    try:
        bpy.app.timers.unregister(_timer)
    except Exception:  # noqa: BLE001 - the timer may already be gone
        pass


def register():
    start()


def unregister():
    stop()


if __name__ == "__main__":
    start()

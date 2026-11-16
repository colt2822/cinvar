"""CINVAR Blender SDK: the bpy side of spatial and visual verification.

Nothing here is estimated. Every number is measured from object matrices, the camera
projection, BVHTree overlaps, scene ray casts, or the pixels of an actual render.

Loaded by a generated driver script in the same directory:

    import sys, os
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import cinvar_bpy_sdk as sdk
    sdk.run(job)

CINVAR writes the job dict as JSON (see preview.rs) and reads back
diagnostics.json plus the four contact-sheet panels.
"""

import json
import math
import os
import traceback

import bmesh
import bpy
from bpy_extras.object_utils import world_to_camera_view
from mathutils import Matrix, Vector
from mathutils.bvhtree import BVHTree

try:
    import numpy as np
except Exception:  # pragma: no cover - Blender ships numpy, but never hard-fail on it
    np = None

GEOMETRY_TYPES = ("MESH", "FONT", "CURVE", "SURFACE", "META", "VOLUME", "GPENCIL")
LATTICE = 3
MAX_OBJECTS = 48
MAX_COLLISION_PAIRS = 64
ALPHA_COVERED = 0.5
LUMA_BINS = 32
EMPTY_COVERAGE = 0.001
OCCLUSION_SAMPLES = 8
BBOX_TOL = 1e-6


def log(message):
    print("CINVAR_SDK %s" % message)


def write_json(path, payload):
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")


def scene_frame_choice(scene, choice):
    """first / middle / final / an explicit frame number, clamped to the scene range."""
    start = int(scene.frame_start)
    end = int(scene.frame_end)
    label = str(choice).strip().lower()
    if label == "first":
        return start
    if label == "middle":
        return (start + end) // 2
    if label == "final":
        return end
    try:
        return max(start, min(end, int(choice)))
    except Exception:
        return start


def geometry_objects(scene):
    out = [obj for obj in scene.objects if obj.type in GEOMETRY_TYPES]
    out.sort(key=lambda o: o.name)
    return out[:MAX_OBJECTS]


def world_bbox(obj):
    """Axis-aligned world-space box of the object, from its 8 local bound_box corners."""
    try:
        corners = [obj.matrix_world @ Vector(c) for c in obj.bound_box]
    except Exception:
        return None
    if not corners:
        return None
    lo = [min(c[i] for c in corners) for i in range(3)]
    hi = [max(c[i] for c in corners) for i in range(3)]
    if (hi[0] - lo[0]) + (hi[1] - lo[1]) + (hi[2] - lo[2]) < BBOX_TOL:
        # Degenerate bound_box (some procedural objects report zeros): fall back to geometry.
        try:
            depsgraph = bpy.context.evaluated_depsgraph_get()
            evaluated = obj.evaluated_get(depsgraph)
            mesh = evaluated.to_mesh()
            points = [obj.matrix_world @ v.co for v in mesh.vertices]
            evaluated.to_mesh_clear()
            if points:
                lo = [min(p[i] for p in points) for i in range(3)]
                hi = [max(p[i] for p in points) for i in range(3)]
        except Exception:
            pass
    return [float(v) for v in lo], [float(v) for v in hi]


def bbox_points(lo, hi, steps=LATTICE):
    points = []
    span = [hi[i] - lo[i] for i in range(3)]
    for i in range(steps):
        for j in range(steps):
            for k in range(steps):
                fx = 0.0 if steps == 1 else i / float(steps - 1)
                fy = 0.0 if steps == 1 else j / float(steps - 1)
                fz = 0.0 if steps == 1 else k / float(steps - 1)
                points.append(
                    Vector((lo[0] + span[0] * fx, lo[1] + span[1] * fy, lo[2] + span[2] * fz))
                )
    return points


def project(scene, cam, point):
    co = world_to_camera_view(scene, cam, point)
    return float(co.x), float(co.y), float(co.z)


def is_inside(x, y, z):
    return z > 0.0 and 0.0 <= x <= 1.0 and 0.0 <= y <= 1.0


def camera_report(scene, cam):
    if cam is None:
        return {
            "name": "",
            "present": False,
            "lens": 0.0,
            "sensor_width": 0.0,
            "location": [0.0, 0.0, 0.0],
            "rotation_euler": [0.0, 0.0, 0.0],
        }
    rot = cam.rotation_euler
    return {
        "name": cam.name,
        "present": True,
        "lens": float(cam.data.lens),
        "sensor_width": float(cam.data.sensor_width),
        "location": [float(v) for v in cam.matrix_world.translation],
        "rotation_euler": [math.degrees(float(v)) for v in rot],
    }


def occlusion_for(scene, cam, obj, points):
    """Share of sample points whose first ray hit is another object."""
    origin = cam.matrix_world.translation
    hits = 0
    samples = 0
    occluders = {}
    for point in points:
        direction = point - origin
        distance = direction.length
        if distance < 1e-6:
            continue
        direction = direction / distance
        samples += 1
        try:
            result, location, normal, index, hit_obj, matrix = scene.ray_cast(
                bpy.context.evaluated_depsgraph_get(),
                origin,
                direction,
                distance=distance * 0.999,
            )
        except Exception:
            result = False
            hit_obj = None
        if result and hit_obj is not None and hit_obj.name != obj.name:
            hits += 1
            occluders[hit_obj.name] = occluders.get(hit_obj.name, 0) + 1
    if not samples:
        return 0.0, None
    occluder = None
    if occluders:
        occluder = sorted(occluders.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    return 100.0 * hits / float(samples), occluder


def bvh_of(obj, depsgraph):
    try:
        if obj.type == "MESH":
            return BVHTree.FromObject(obj, depsgraph)
        evaluated = obj.evaluated_get(depsgraph)
        mesh = evaluated.to_mesh()
        verts = [v.co.copy() for v in mesh.vertices]
        faces = [list(p.vertices) for p in mesh.polygons]
        evaluated.to_mesh_clear()
        if not verts or not faces:
            return None
        return BVHTree.FromPolygons(verts, faces)
    except Exception:
        return None


def aabb_overlap(a, b):
    lo = [max(a[0][i], b[0][i]) for i in range(3)]
    hi = [min(a[1][i], b[1][i]) for i in range(3)]
    if any(hi[i] <= lo[i] for i in range(3)):
        return 0.0
    inter = 1.0
    for i in range(3):
        inter *= hi[i] - lo[i]
    vol_a = max(BBOX_TOL, (a[1][0] - a[0][0]) * (a[1][1] - a[0][1]) * (a[1][2] - a[0][2]))
    vol_b = max(BBOX_TOL, (b[1][0] - b[0][0]) * (b[1][1] - b[0][1]) * (b[1][2] - b[0][2]))
    return float(inter / min(vol_a, vol_b))


def collect(scene, frame, floor_z=0.0):
    scene.frame_set(int(frame))
    bpy.context.view_layer.update()
    depsgraph = bpy.context.evaluated_depsgraph_get()
    cam = scene.camera
    objects = geometry_objects(scene)
    boxes = {}
    reports = []
    layout = scene.render
    for obj in objects:
        box = world_bbox(obj)
        if box is None:
            continue
        boxes[obj.name] = box
        inside = 0
        behind = 0
        xs = []
        ys = []
        inside_points = []
        points = bbox_points(box[0], box[1])
        for point in points:
            x, y, z = project(scene, cam, point) if cam is not None else (0.0, 0.0, 0.0)
            if z <= 0.0:
                behind += 1
                continue
            xs.append(x)
            ys.append(y)
            if is_inside(x, y, z):
                inside += 1
                inside_points.append(point)
        total = len(points)
        inside_pct = 100.0 * inside / float(total)
        occluded_pct, occluder = (0.0, None)
        if cam is not None and inside_points:
            occluded_pct, occluder = occlusion_for(
                scene, cam, obj, inside_points[:OCCLUSION_SAMPLES]
            )
        penetration = 0.0
        if box[0][2] < floor_z:
            penetration = float(floor_z - box[0][2])
        reports.append(
            {
                "name": obj.name,
                "kind": obj.type,
                "bbox_min": box[0],
                "bbox_max": box[1],
                "center": [(box[0][i] + box[1][i]) * 0.5 for i in range(3)],
                "inside_pct": inside_pct,
                "outside_pct": 100.0 - inside_pct,
                "samples_inside": inside,
                "samples_total": total,
                "samples_behind_camera": behind,
                "behind_camera": behind == total,
                "visible": inside > 0,
                "occluded_pct": occluded_pct,
                "occluder": occluder,
                "floor_penetration": penetration,
                "screen_min": [min(xs), min(ys)] if xs else None,
                "screen_max": [max(xs), max(ys)] if ys else None,
            }
        )
    intersections = []
    names = sorted(boxes.keys())
    bvhs = {}
    for i in range(len(names)):
        for j in range(i + 1, len(names)):
            a = names[i]
            b = names[j]
            overlap = aabb_overlap(boxes[a], boxes[b])
            if overlap <= 0.0:
                continue
            pairs = 0
            if len(bvhs) < MAX_COLLISION_PAIRS:
                if a not in bvhs:
                    bvhs[a] = bvh_of(scene.objects[a], depsgraph)
                if b not in bvhs:
                    bvhs[b] = bvh_of(scene.objects[b], depsgraph)
                if bvhs.get(a) is not None and bvhs.get(b) is not None:
                    try:
                        pairs = len(bvhs[a].overlap(bvhs[b]))
                    except Exception:
                        pairs = 0
            intersections.append(
                {
                    "a": a,
                    "b": b,
                    "bbox_overlap": overlap,
                    "mesh_pairs": int(pairs),
                }
            )
    return {
        "frame": int(scene.frame_current),
        "scene": {
            "name": scene.name,
            "engine": scene.render.engine,
            "resolution": [int(layout.resolution_x), int(layout.resolution_y)],
            "fps": float(layout.fps),
            "frame_start": int(scene.frame_start),
            "frame_end": int(scene.frame_end),
        },
        "camera": camera_report(scene, cam),
        "objects": reports,
        "intersections": intersections,
        "floor_z": float(floor_z),
    }


def image_stats(path):
    """Luminance histogram, exposure and alpha coverage of one rendered PNG."""
    result = {
        "path": path,
        "luminance_hist": [0.0] * LUMA_BINS,
        "mean_luma": 0.0,
        "over_exposed_pct": 0.0,
        "under_exposed_pct": 0.0,
        "alpha_coverage": 0.0,
        "covered_pixels": 0,
        "total_pixels": 0,
        "empty": True,
    }
    if not os.path.isfile(path):
        return result
    image = bpy.data.images.load(path, check_existing=False)
    try:
        try:
            image.colorspace_settings.name = "Non-Color"
        except Exception:
            pass
        width, height = int(image.size[0]), int(image.size[1])
        total = width * height
        result["total_pixels"] = total
        if np is not None:
            buffer = np.empty(total * 4, dtype=np.float32)
            image.pixels.foreach_get(buffer)
            arr = buffer.reshape(total, 4)
            alpha = arr[:, 3]
            covered = alpha >= ALPHA_COVERED
            count = int(covered.sum())
            result["covered_pixels"] = count
            result["alpha_coverage"] = float(count) / float(total) if total else 0.0
            luma = 0.2126 * arr[:, 0] + 0.7152 * arr[:, 1] + 0.0722 * arr[:, 2]
            values = luma[covered] if count else np.zeros(1, dtype=np.float32)
            hist, _ = np.histogram(values, bins=LUMA_BINS, range=(0.0, 1.0))
            denominator = float(max(1, int(hist.sum())))
            result["luminance_hist"] = [float(h) / denominator for h in hist]
            result["mean_luma"] = float(values.mean())
            result["over_exposed_pct"] = 100.0 * float((values > 0.98).sum()) / denominator
            result["under_exposed_pct"] = 100.0 * float((values < 0.02).sum()) / denominator
        else:  # pragma: no cover - sampled fallback without numpy
            pixels = list(image.pixels)
            step = 97
            hist = [0] * LUMA_BINS
            covered = 0
            sampled = 0
            luma_sum = 0.0
            for index in range(0, total, step):
                base = index * 4
                alpha = pixels[base + 3]
                sampled += 1
                if alpha < ALPHA_COVERED:
                    continue
                covered += 1
                luma = (
                    0.2126 * pixels[base]
                    + 0.7152 * pixels[base + 1]
                    + 0.0722 * pixels[base + 2]
                )
                luma_sum += luma
                hist[min(LUMA_BINS - 1, int(luma * LUMA_BINS))] += 1
            denominator = float(max(1, covered))
            result["covered_pixels"] = int(total * covered / float(max(1, sampled)))
            result["alpha_coverage"] = covered / float(max(1, sampled))
            result["luminance_hist"] = [h / denominator for h in hist]
            result["mean_luma"] = luma_sum / denominator
    finally:
        bpy.data.images.remove(image)
    result["empty"] = result["alpha_coverage"] < EMPTY_COVERAGE
    return result


def setup_workbench(scene, size, transparent=True):
    render = scene.render
    saved = {
        "engine": render.engine,
        "resolution_x": render.resolution_x,
        "resolution_y": render.resolution_y,
        "resolution_percentage": render.resolution_percentage,
        "film_transparent": render.film_transparent,
        "file_format": render.image_settings.file_format,
        "color_mode": render.image_settings.color_mode,
        "camera": scene.camera.name if scene.camera else None,
        "shading_type": scene.display.shading.type,
        "color_type": scene.display.shading.color_type,
        "light": scene.display.shading.light,
        "aa": scene.display.render_aa,
        "outline": scene.display.shading.show_object_outline,
    }
    render.engine = "BLENDER_WORKBENCH"
    scene.display.render_aa = "8"
    scene.display.shading.light = "STUDIO"
    scene.display.shading.type = "SOLID"
    scene.display.shading.color_type = "MATERIAL"
    scene.display.shading.show_object_outline = True
    render.resolution_percentage = 100
    render.image_settings.file_format = "PNG"
    render.image_settings.color_mode = "RGBA"
    render.film_transparent = bool(transparent)
    render.use_stamp = False
    return saved


def restore_scene(scene, saved):
    render = scene.render
    render.engine = saved["engine"]
    render.resolution_x = saved["resolution_x"]
    render.resolution_y = saved["resolution_y"]
    render.resolution_percentage = saved["resolution_percentage"]
    render.film_transparent = saved["film_transparent"]
    render.image_settings.file_format = saved["file_format"]
    render.image_settings.color_mode = saved["color_mode"]
    scene.display.shading.type = saved["shading_type"]
    scene.display.shading.color_type = saved["color_type"]
    scene.display.shading.light = saved["light"]
    scene.display.render_aa = saved["aa"]
    scene.display.shading.show_object_outline = saved["outline"]
    if saved["camera"]:
        cam = scene.objects.get(saved["camera"])
        if cam is not None:
            scene.camera = cam


def render_still(scene, path):
    scene.render.filepath = path
    bpy.ops.render.render(write_still=True)
    return path


def add_ortho_camera(scene, name, location, rotation, ortho_scale):
    data = bpy.data.cameras.new(name)
    data.type = "ORTHO"
    data.ortho_scale = max(0.1, ortho_scale)
    obj = bpy.data.objects.new(name, data)
    obj.location = location
    obj.rotation_euler = rotation
    scene.collection.objects.link(obj)
    bpy.context.view_layer.update()
    return obj


def add_bbox_wireframes(scene, objects, boxes, flagged):
    """Thin wire boxes around every measured object, so view 4 shows the real bounds."""
    created = []
    for obj in objects:
        box = boxes.get(obj.name)
        if box is None:
            continue
        lo, hi = box
        size = [max(1e-4, hi[i] - lo[i]) for i in range(3)]
        center = [(lo[i] + hi[i]) * 0.5 for i in range(3)]
        bpy.ops.mesh.primitive_cube_add(size=1.0, location=center)
        cube = bpy.context.active_object
        cube.name = "CINVAR_BBOX_" + obj.name
        cube.scale = (size[0], size[1], size[2])
        modifier = cube.modifiers.new(name="CINVAR_WIRE", type="WIREFRAME")
        modifier.thickness = max(0.004, 0.01 * max(size))
        modifier.use_even_offset = False
        cube.color = (1.0, 0.25, 0.2, 1.0) if obj.name in flagged else (0.35, 0.9, 0.45, 1.0)
        created.append(cube)
    bpy.context.view_layer.update()
    return created


def delete_objects(scene, objects):
    for obj in objects:
        try:
            bpy.data.objects.remove(obj, do_unlink=True)
        except Exception:
            pass


def even(value):
    return max(2, int(round(value)) - int(round(value)) % 2)


def render_views(scene, out_dir, size, flagged=None):
    """Four diagnostic panels: active camera, top ortho, side ortho, wireframe with bounds."""
    flagged = flagged or set()
    view_size = max(256, int(size))
    camera = scene.camera
    aspect = 1.0
    if scene.render.resolution_y:
        aspect = float(scene.render.resolution_x) / float(scene.render.resolution_y)
    camera_height = even(view_size / max(0.1, aspect))
    saved = setup_workbench(scene, view_size, transparent=True)
    paths = {}
    try:
        if camera is not None:
            scene.render.resolution_x = view_size
            scene.render.resolution_y = camera_height
            paths["camera"] = render_still(scene, os.path.join(out_dir, "view_camera.png"))

        objects = geometry_objects(scene)
        boxes = {}
        for obj in objects:
            box = world_bbox(obj)
            if box is not None:
                boxes[obj.name] = box
        if boxes:
            lo = [min(b[0][i] for b in boxes.values()) for i in range(3)]
            hi = [max(b[1][i] for b in boxes.values()) for i in range(3)]
        else:
            lo, hi = [-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]
        center = [(lo[i] + hi[i]) * 0.5 for i in range(3)]
        span = [max(0.1, hi[i] - lo[i]) for i in range(3)]
        reach = 2.0 * max(span) + 2.0

        scene.render.resolution_x = view_size
        scene.render.resolution_y = view_size
        scene.render.resolution_percentage = 100

        top = add_ortho_camera(
            scene,
            "CINVAR_TOP",
            (center[0], center[1], center[2] + reach),
            (0.0, 0.0, 0.0),
            max(span[0], span[1]) * 1.15,
        )
        scene.camera = top
        paths["top"] = render_still(scene, os.path.join(out_dir, "view_top.png"))

        side = add_ortho_camera(
            scene,
            "CINVAR_SIDE",
            (center[0] + reach, center[1], center[2]),
            (math.radians(90.0), 0.0, math.radians(90.0)),
            max(span[1], span[2]) * 1.15,
        )
        scene.camera = side
        paths["side"] = render_still(scene, os.path.join(out_dir, "view_side.png"))

        if camera is not None:
            scene.render.resolution_x = view_size
            scene.render.resolution_y = camera_height
            boxes_created = add_bbox_wireframes(scene, objects, boxes, flagged)
            scene.display.shading.type = "WIREFRAME"
            scene.display.shading.color_type = "OBJECT"
            scene.camera = camera
            paths["wire"] = render_still(scene, os.path.join(out_dir, "view_wire.png"))
            delete_objects(scene, boxes_created)
        delete_objects(scene, [top, side])
    finally:
        restore_scene(scene, saved)
    return paths


def correction_ops(diagnostics):
    """Bounded, numeric corrections derived from the measured diagnostics."""
    ops = []
    if not diagnostics.get("camera", {}).get("present"):
        return ops
    worst = 0.0
    for obj in diagnostics.get("objects", []):
        for axis in (0, 1):
            low = obj.get("screen_min")
            high = obj.get("screen_max")
            if not low or not high:
                continue
            extent = max(abs(low[axis] - 0.5), abs(high[axis] - 0.5)) * 2.0
            worst = max(worst, extent)
    if worst > 1.0:
        factor = min(2.5, max(1.05, worst / 0.9))
        ops.append(
            {
                "op": "camera_dolly",
                "factor": round(factor, 4),
                "reason": "objects reach %.0f%% of the frame; pull the camera back to fit them"
                % (100.0 * worst),
            }
        )
    for obj in diagnostics.get("objects", []):
        depth = obj.get("floor_penetration", 0.0)
        if depth > 1e-4:
            ops.append(
                {
                    "op": "raise_object",
                    "target": obj["name"],
                    "delta": round(depth + 0.01, 5),
                    "reason": "%.3f m below the floor plane" % depth,
                }
            )
    # Collisions and occlusions are reported but never auto-edited: which object gives way is
    # an editorial decision, and silently sliding geometry would corrupt a scene the caller
    # still owns. Only the camera distance and object height are numeric, reversible fixes.
    return ops


def apply_corrections(scene, ops):
    applied = []
    for op in ops:
        kind = op.get("op")
        if kind == "camera_dolly":
            cam = scene.camera
            if cam is None:
                continue
            factor = float(op.get("factor", 1.0))
            forward = cam.matrix_world.to_quaternion() @ Vector((0.0, 0.0, -1.0))
            position = cam.matrix_world.translation.copy()
            distance = max(0.25, -position.dot(forward))
            cam.location = position - forward * (distance * (factor - 1.0))
            applied.append(dict(op))
        elif kind == "raise_object":
            obj = scene.objects.get(op.get("target", ""))
            if obj is None:
                continue
            obj.location = (
                obj.location[0],
                obj.location[1],
                obj.location[2] + float(op.get("delta", 0.0)),
            )
            applied.append(dict(op))
    if applied:
        bpy.context.view_layer.update()
    return applied


def main(job):
    out_dir = job["out_dir"]
    frame_choice = job.get("frame", "middle")
    size = int(job.get("size", 512))
    max_rounds = max(1, int(job.get("max_rounds", 1)))
    floor_z = float(job.get("floor_z", 0.0))
    mode = job.get("mode", "check")
    os.makedirs(out_dir, exist_ok=True)
    scene = bpy.context.scene
    frame = scene_frame_choice(scene, frame_choice)
    rounds = []
    diagnostics = collect(scene, frame, floor_z)
    for attempt in range(1, max_rounds + 1):
        rounds.append(
            {
                "round": attempt,
                "frame": diagnostics["frame"],
                "diagnostics": diagnostics,
            }
        )
        if mode != "correct" or attempt == max_rounds:
            break
        ops = correction_ops(diagnostics)
        if not ops:
            break
        applied = apply_corrections(scene, ops)
        rounds[-1]["corrections"] = applied
        if not applied:
            break
        log("round %d applied %d correction(s)" % (attempt, len(applied)))
        diagnostics = collect(scene, frame, floor_z)
    scene.frame_set(diagnostics["frame"])
    bpy.context.view_layer.update()
    flagged = {
        report["name"]
        for report in diagnostics.get("objects", [])
        if report.get("outside_pct", 0.0) > 0.0
    }
    views = render_views(scene, out_dir, size, flagged)
    image = image_stats(views.get("camera", "")) if views.get("camera") else image_stats("")
    diagnostics["image"] = image
    diagnostics["views"] = views
    write_json(os.path.join(out_dir, "diagnostics.json"), diagnostics)
    write_json(
        os.path.join(out_dir, "rounds.json"),
        {"mode": mode, "max_rounds": max_rounds, "rounds": rounds},
    )
    log("frame %d objects %d views %d" % (diagnostics["frame"], len(diagnostics["objects"]), len(views)))
    return 0


def run(job):
    try:
        main(job)
    except Exception:
        detail = traceback.format_exc()
        log("FAILED\n" + detail)
        target = os.path.join(job.get("out_dir", "."), "error.json")
        try:
            write_json(target, {"error": detail.strip().splitlines()[-1], "traceback": detail})
        except Exception:
            pass
        raise

# ---------------------------------------------------------------------------
# Argon 5C: declarative scene construction.
#
# A CinvarBlenderScene (defined in scene.rs) arrives here as JSON *data*. No
# caller-supplied python is ever evaluated: every operator name, socket name and
# engine id is chosen below, from tables this file owns, and every operator that
# wants a UI context goes through sc_guarded_op() first.
# ---------------------------------------------------------------------------

SCENE_MODE_BUILD = "build"
SCENE_MODE_VERIFY = "verify"
SCENE_MODE_STILL = "still"

# Principled BSDF input roles, newest Blender name first. Blender 4.0 renamed
# Emission/Emission Strength, Transmission, Specular and Subsurface; 5.0 keeps
# the 4.x names. The Rust side owns the same table and ships it in the job as
# socket_aliases (a unit test keeps the two copies in step).
PRINCIPLED_ALIASES = {
    "base_color": ("Base Color",),
    "metallic": ("Metallic",),
    "roughness": ("Roughness",),
    "ior": ("IOR",),
    "alpha": ("Alpha",),
    "emission_color": ("Emission Color", "Emission"),
    "emission_strength": ("Emission Strength",),
    "transmission": ("Transmission Weight", "Transmission"),
    "specular": ("Specular IOR Level", "Specular"),
    "subsurface": ("Subsurface Weight", "Subsurface"),
}

LIGHT_KINDS = {"point": "POINT", "area": "AREA", "sun": "SUN", "spot": "SPOT"}

# Preset light intensities are quoted in watts at this distance; every preset
# rescales them by inverse square so a 4 cm logo and a 4 m product lit the same.
SCENE_REFERENCE_RADIUS = 3.0

ASSET_IMPORTERS = {
    ".glb": ("import_scene.gltf",),
    ".gltf": ("import_scene.gltf",),
    ".fbx": ("import_scene.fbx",),
    ".obj": ("wm.obj_import", "import_scene.obj"),
    ".svg": ("import_curve.svg",),
}


def blender_version():
    version = getattr(bpy.app, "version", (0, 0, 0))
    return (int(version[0]), int(version[1]), int(version[2]) if len(version) > 2 else 0)


def blender_version_text():
    return ".".join(str(part) for part in blender_version())


def eevee_engine_name(version):
    """EEVEE's engine id per generation: EEVEE (<=4.1), EEVEE Next (4.2-4.x), EEVEE (5.0+)."""
    major = int(version[0])
    minor = int(version[1])
    if major >= 5:
        return "BLENDER_EEVEE"
    if major == 4 and minor >= 2:
        return "BLENDER_EEVEE_NEXT"
    return "BLENDER_EEVEE"


def engine_ids(scene):
    """The engine ids this Blender actually offers, read from the RNA enum."""
    prop = scene.render.bl_rna.properties.get("engine")
    if prop is None:
        return ()
    return tuple(item.identifier for item in prop.enum_items)


def resolve_engine(scene, requested, warnings):
    """Maps a friendly engine name to an engine id this Blender has, or warns and falls back."""
    wanted = str(requested or "eevee").strip().lower()
    available = engine_ids(scene)
    if wanted in ("cycles",):
        candidates = ["CYCLES"]
    elif wanted == "workbench":
        candidates = ["BLENDER_WORKBENCH"]
    elif wanted in ("eevee", "evee", "eevee_next", "blender_eevee", "blender_eevee_next"):
        candidates = [eevee_engine_name(blender_version()), "BLENDER_EEVEE_NEXT", "BLENDER_EEVEE"]
    elif wanted.upper() in available:
        candidates = [wanted.upper()]
    else:
        candidates = [eevee_engine_name(blender_version()), "BLENDER_EEVEE", "CYCLES"]
        warnings.append("unknown engine %r; falling back to %s" % (wanted, candidates[0]))
    for candidate in candidates:
        if not available or candidate in available:
            return candidate
    warnings.append(
        "none of %s is available in Blender %s" % (list(candidates), blender_version_text())
    )
    return available[0] if available else "BLENDER_EEVEE"


def principled_socket(bsdf, role, aliases):
    """The socket for one Principled BSDF role in this Blender, or None."""
    inputs = bsdf.inputs
    for name in aliases.get(role, ()):
        try:
            if name in inputs:
                return inputs[name]
        except (KeyError, TypeError):
            continue
    return None


def set_principled(bsdf, role, value, aliases):
    """Sets one Principled input through the rename shim; False when this Blender has no such socket."""
    socket = principled_socket(bsdf, role, aliases)
    if socket is None:
        return False
    try:
        socket.default_value = value
    except (TypeError, ValueError):
        return False
    return True


def srgb_to_linear(value):
    channel = max(0.0, min(1.0, float(value)))
    if channel <= 0.04045:
        return channel / 12.92
    return ((channel + 0.055) / 1.055) ** 2.4


def linear_rgba(color, alpha=1.0):
    """'#rrggbb', '#rgb', or a list of 0..1 / 0..255 channels -> linear RGBA."""
    if color is None:
        return (0.8, 0.8, 0.8, float(alpha))
    if isinstance(color, str):
        text = color.strip().lstrip("#")
        if len(text) == 3:
            text = "".join(part * 2 for part in text)
        try:
            channels = [int(text[i : i + 2], 16) / 255.0 for i in (0, 2, 4)]
        except (ValueError, IndexError):
            channels = [0.8, 0.8, 0.8]
    else:
        channels = [float(value) for value in color][:3]
        if any(channel > 1.0 for channel in channels):
            channels = [channel / 255.0 for channel in channels]
    return tuple(srgb_to_linear(channel) for channel in channels) + (float(alpha),)


def linear_rgb(color):
    return linear_rgba(color)[:3]


# ---------------------------------------------------------------------------
# Operator guards: 4.x+ wants an explicit context override for UI operators.
# ---------------------------------------------------------------------------


def sc_operator(name):
    target = bpy.ops
    for part in name.split("."):
        target = getattr(target, part, None)
        if target is None:
            return None
    return target


def sc_op_context(scene):
    """A context override for UI operators, or None when the operator needs none."""
    if not hasattr(bpy.context, "temp_override"):
        return None
    window = getattr(bpy.context, "window", None)
    if window is None:
        return {"scene": scene}
    override = {"window": window, "screen": window.screen}
    for area in getattr(window.screen, "areas", ()):
        if area.type == "VIEW_3D":
            override["area"] = area
            for region in area.regions:
                if region.type == "WINDOW":
                    override["region"] = region
            break
    return override


def sc_guarded_op(scene, name, kwargs, warnings, label):
    """Runs one operator that can require a UI context, with a guard and an explicit warning."""
    operator = sc_operator(name)
    if operator is None:
        warnings.append("%s: this Blender has no %s operator" % (label, name))
        return None
    override = sc_op_context(scene)
    try:
        if override and hasattr(bpy.context, "temp_override"):
            with bpy.context.temp_override(**override):
                result = operator(**dict(kwargs or {}))
        else:
            result = operator(**dict(kwargs or {}))
    except Exception as exc:
        warnings.append("%s: %s failed (%s)" % (label, name, exc))
        return None
    if isinstance(result, set) and "FINISHED" not in result:
        warnings.append("%s: %s returned %s" % (label, name, sorted(result)))
        return None
    return result


# ---------------------------------------------------------------------------
# Geometry generators: real vertex data, no bpy.ops primitives.
# ---------------------------------------------------------------------------


def sc_gen_cube(size=1.0):
    half = float(size) * 0.5
    verts = [
        (-half, -half, -half),
        (half, -half, -half),
        (-half, half, -half),
        (half, half, -half),
        (-half, -half, half),
        (half, -half, half),
        (-half, half, half),
        (half, half, half),
    ]
    faces = [
        (0, 1, 3, 2),
        (4, 6, 7, 5),
        (0, 4, 5, 1),
        (2, 3, 7, 6),
        (0, 2, 6, 4),
        (1, 5, 7, 3),
    ]
    return verts, faces


def sc_gen_plane(size=1.0):
    half = float(size) * 0.5
    verts = [(-half, -half, 0.0), (half, -half, 0.0), (half, half, 0.0), (-half, half, 0.0)]
    return verts, [(0, 1, 2, 3)]


def sc_gen_sphere(radius=0.5, segments=32, rings=16):
    radius = float(radius)
    segments = max(3, int(segments))
    rings = max(2, int(rings))
    verts = [(0.0, 0.0, radius)]
    for ring in range(1, rings):
        phi = math.pi * ring / rings
        z = radius * math.cos(phi)
        r = radius * math.sin(phi)
        for segment in range(segments):
            theta = 2.0 * math.pi * segment / segments
            verts.append((r * math.cos(theta), r * math.sin(theta), z))
    verts.append((0.0, 0.0, -radius))
    bottom = len(verts) - 1
    faces = []
    for segment in range(segments):
        nxt = (segment + 1) % segments
        faces.append((0, 1 + segment, 1 + nxt))
    for ring in range(rings - 2):
        base = 1 + ring * segments
        upper = base + segments
        for segment in range(segments):
            nxt = (segment + 1) % segments
            faces.append((base + segment, upper + segment, upper + nxt, base + nxt))
    last = 1 + (rings - 2) * segments
    for segment in range(segments):
        nxt = (segment + 1) % segments
        faces.append((bottom, last + nxt, last + segment))
    return verts, faces


def sc_gen_cylinder(radius=0.5, depth=1.0, vertices=32):
    radius = float(radius)
    half = float(depth) * 0.5
    count = max(3, int(vertices))
    verts = []
    for level in (-1.0, 1.0):
        for index in range(count):
            angle = 2.0 * math.pi * index / count
            verts.append((radius * math.cos(angle), radius * math.sin(angle), half * level))
    faces = []
    for index in range(count):
        nxt = (index + 1) % count
        faces.append((index, nxt, count + nxt, count + index))
    faces.append(tuple(range(count - 1, -1, -1)))
    faces.append(tuple(range(count, 2 * count)))
    return verts, faces


def sc_gen_torus(major_radius=0.5, minor_radius=0.15, major_segments=48, minor_segments=16):
    major = float(major_radius)
    minor = float(minor_radius)
    major_segments = max(3, int(major_segments))
    minor_segments = max(3, int(minor_segments))
    verts = []
    for index in range(major_segments):
        u = 2.0 * math.pi * index / major_segments
        for other in range(minor_segments):
            v = 2.0 * math.pi * other / minor_segments
            ring = major + minor * math.cos(v)
            verts.append((ring * math.cos(u), ring * math.sin(u), minor * math.sin(v)))
    faces = []
    for index in range(major_segments):
        nxt = (index + 1) % major_segments
        for other in range(minor_segments):
            following = (other + 1) % minor_segments
            faces.append(
                (
                    index * minor_segments + other,
                    index * minor_segments + following,
                    nxt * minor_segments + following,
                    nxt * minor_segments + other,
                )
            )
    return verts, faces


def sc_gen_cyclorama(floor_radius=12.0, curve_radius=3.0, wall_height=6.0, segments=64):
    """A cyclorama room: floor, quarter-round cove, wall — revolved around Z.

    Revolved rather than flat, because a turntable looks at the subject from every azimuth and
    a single backdrop wall would end up behind the camera half way round.
    """
    floor_radius = max(float(floor_radius), 0.5)
    curve_radius = max(float(curve_radius), 1e-3)
    wall_height = max(float(wall_height), curve_radius + 1e-3)
    segments = max(int(segments), 8)
    profile = []
    arc = 16
    for index in range(arc + 1):
        theta = (math.pi * 0.5) * index / arc
        profile.append(
            (floor_radius + curve_radius * math.sin(theta), curve_radius * (1.0 - math.cos(theta)))
        )
    for index in range(1, 3):
        profile.append(
            (floor_radius + curve_radius, curve_radius + wall_height * index / 2.0)
        )
    rings = len(profile)
    verts = []
    for segment in range(segments):
        phi = 2.0 * math.pi * segment / segments
        for (radius, height) in profile:
            verts.append((radius * math.cos(phi), radius * math.sin(phi), height))
    faces = []
    for segment in range(segments):
        following = (segment + 1) % segments
        for level in range(rings - 1):
            faces.append(
                (
                    segment * rings + level,
                    segment * rings + level + 1,
                    following * rings + level + 1,
                    following * rings + level,
                )
            )
    centre = len(verts)
    verts.append((0.0, 0.0, 0.0))
    for segment in range(segments):
        following = (segment + 1) % segments
        faces.append((centre, following * rings, segment * rings))
    return verts, faces


def sc_face_towards(mesh_object, point):
    """Orients a shell's faces towards a point: an interior-facing cyclorama is lit, not black."""
    mesh = mesh_object.data
    if not len(mesh.polygons):
        return False
    target = Vector(point)
    inward = sum(1 for poly in mesh.polygons if poly.normal.dot(target - poly.center) > 0.0)
    if inward * 2 < len(mesh.polygons):
        mesh.flip_normals()
        mesh.update()
        return True
    return False


def sc_mesh_object(scene, name, verts, faces):
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata([tuple(v) for v in verts], [], [tuple(f) for f in faces])
    mesh.validate(verbose=False)
    if len(mesh.polygons):
        handle = bmesh.new()
        handle.from_mesh(mesh)
        bmesh.ops.recalc_face_normals(handle, faces=handle.faces)
        handle.to_mesh(mesh)
        handle.free()
    mesh.update()
    obj = bpy.data.objects.new(name, mesh)
    scene.collection.objects.link(obj)
    return obj


def sc_text_object(scene, name, spec):
    curve = bpy.data.curves.new(name, type="FONT")
    curve.body = str(spec.get("body", "CINVAR"))
    curve.size = float(spec.get("size", 1.0))
    curve.extrude = max(float(spec.get("extrude", 0.05)), 0.0)
    curve.bevel_depth = max(float(spec.get("bevel_depth", 0.0)), 0.0)
    curve.align_x = str(spec.get("align_x", "CENTER")).upper()
    curve.align_y = str(spec.get("align_y", "CENTER")).upper()
    obj = bpy.data.objects.new(name, curve)
    scene.collection.objects.link(obj)
    return obj


def sc_apply_transform(obj, transform):
    transform = transform or {}
    obj.location = tuple(float(value) for value in transform.get("location", (0.0, 0.0, 0.0)))
    obj.rotation_euler = tuple(
        math.radians(float(value)) for value in transform.get("rotation_deg", (0.0, 0.0, 0.0))
    )
    obj.scale = tuple(float(value) for value in transform.get("scale", (1.0, 1.0, 1.0)))


# ---------------------------------------------------------------------------
# Materials
# ---------------------------------------------------------------------------


def sc_material(name, spec, aliases, warnings):
    material = bpy.data.materials.new(name)
    material.use_nodes = True
    bsdf = material.node_tree.nodes.get("Principled BSDF")
    if bsdf is None:
        warnings.append("material %s has no Principled BSDF node; left at default" % name)
        return material
    alpha = float(spec.get("alpha", 1.0))
    set_principled(bsdf, "base_color", linear_rgba(spec.get("base_color", "#cccccc"), alpha), aliases)
    set_principled(bsdf, "metallic", float(spec.get("metallic", 0.0)), aliases)
    set_principled(bsdf, "roughness", float(spec.get("roughness", 0.5)), aliases)
    if spec.get("specular") is not None:
        set_principled(bsdf, "specular", float(spec["specular"]), aliases)
    if spec.get("ior") is not None:
        set_principled(bsdf, "ior", float(spec["ior"]), aliases)
    transmission = float(spec.get("transmission", 0.0))
    if transmission > 0.0:
        if not set_principled(bsdf, "transmission", transmission, aliases):
            warnings.append("material %s: this Blender has no transmission socket" % name)
    if alpha < 1.0:
        set_principled(bsdf, "alpha", alpha, aliases)
        # EEVEE Next (4.2+) replaced blend_method with surface_render_method.
        for attribute, value in (("blend_method", "BLEND"), ("surface_render_method", "BLENDED")):
            if hasattr(material, attribute):
                try:
                    setattr(material, attribute, value)
                except (TypeError, ValueError):
                    pass
    emission = spec.get("emission")
    if emission:
        set_principled(bsdf, "emission_color", linear_rgba(emission.get("color", "#ffffff")), aliases)
        set_principled(bsdf, "emission_strength", float(emission.get("strength", 1.0)), aliases)
    return material


def sc_assign_material(obj, material):
    if obj.data is None or not hasattr(obj.data, "materials"):
        return False
    if obj.data.materials:
        obj.data.materials[0] = material
    else:
        obj.data.materials.append(material)
    return True


# ---------------------------------------------------------------------------
# Asset imports, normalisation, texture audit
# ---------------------------------------------------------------------------


def sc_append_blend(scene, path, warnings):
    """Appends every object from another .blend, materials included, without bpy.ops."""
    try:
        with bpy.data.libraries.load(path, link=False) as (source, target):
            target.objects = list(source.objects)
        loaded = [obj for obj in target.objects if obj is not None]
    except Exception as exc:
        warnings.append("append %s failed (%s)" % (path, exc))
        return []
    for obj in loaded:
        if obj.name not in scene.collection.objects:
            try:
                scene.collection.objects.link(obj)
            except RuntimeError:
                pass
    return loaded


def sc_svg_extrude(scene, objects, depth, warnings):
    """Extrudes imported SVG curves into real mesh geometry.

    extrude_depth is measured in logo widths: the imported paths are first canonicalised so
    their longest in-plane extent is 1.0, which keeps the thickness meaningful whatever scale
    Blender's SVG importer chose (it maps 1 SVG unit to roughly 0.00024 m).
    """
    depth = float(depth)
    if depth <= 0.0:
        warnings.append("svg extrude_depth <= 0; using 0.01 so the paths keep a volume")
        depth = 0.01
    curves = [obj for obj in objects if obj.type == "CURVE"]
    if curves:
        bpy.context.view_layer.update()
        low, high = sc_world_bbox_of(curves)
        if low is not None:
            span = max(high[i] - low[i] for i in range(3))
            if span > 1e-9:
                centre = Vector([(low[i] + high[i]) * 0.5 for i in range(3)])
                rescale = (
                    Matrix.Translation(centre)
                    @ Matrix.Scale(1.0 / span, 4)
                    @ Matrix.Translation(-centre)
                )
                # SVG paths arrive flat on the floor (XY). A logo is presented standing up,
                # so the whole set is turned 90 degrees about X: it then faces the camera and
                # the extrusion becomes depth into the scene instead of a slab on the ground.
                upright = (
                    Matrix.Translation(centre)
                    @ Matrix.Rotation(math.radians(90.0), 4, "X")
                    @ Matrix.Translation(-centre)
                )
                world = upright @ rescale
                for obj in curves:
                    if obj.parent is None:
                        obj.matrix_world = world @ obj.matrix_world
                bpy.context.view_layer.update()
    for obj in curves:
        data = obj.data
        # data.extrude is in the curve's local units, and the canonicalising matrix above
        # scales those units: divide it out so depth is the world thickness in logo widths.
        # (A 2D curve keeps its shape in the local XY plane, so the transform must live on the
        # object, not in the curve data.)
        scale = max([abs(value) for value in obj.matrix_world.to_scale()] or [1.0])
        try:
            data.extrude = depth * 0.5 / max(scale, 1e-9)
            data.fill_mode = "BOTH"
            data.offset = 0.0
            if getattr(data, "bevel_depth", 0.0):
                data.bevel_depth = 0.0
        except Exception as exc:
            warnings.append("svg %s: could not set extrude (%s)" % (obj.name, exc))
    bpy.context.view_layer.update()
    depsgraph = bpy.context.evaluated_depsgraph_get()
    out = []
    for obj in objects:
        if obj.type != "CURVE":
            out.append(obj)
            continue
        mesh = None
        try:
            evaluated = obj.evaluated_get(depsgraph)
            mesh = bpy.data.meshes.new_from_object(evaluated, depsgraph=depsgraph)
        except Exception as exc:
            warnings.append("svg %s: curve to mesh failed (%s)" % (obj.name, exc))
        if mesh is None or len(mesh.vertices) == 0:
            warnings.append("svg %s: extrusion produced no geometry" % obj.name)
            if mesh is not None:
                bpy.data.meshes.remove(mesh)
            out.append(obj)
            continue
        converted = bpy.data.objects.new(obj.name + "_mesh", mesh)
        converted.matrix_world = obj.matrix_world.copy()
        for material in getattr(obj.data, "materials", ()):
            if material is not None and material.name not in [m.name for m in mesh.materials]:
                mesh.materials.append(material)
        scene.collection.objects.link(converted)
        original_name = obj.name
        bpy.data.objects.remove(obj, do_unlink=True)
        converted.name = original_name
        out.append(converted)
    return out


def sc_import_asset(scene, spec, warnings):
    """Imports one declared asset and returns the objects it brought in."""
    name = str(spec.get("name") or os.path.basename(str(spec.get("path", ""))))
    path = str(spec.get("path", ""))
    if not path or not os.path.isfile(path):
        raise RuntimeError("asset %s: no such file %s" % (name, path))
    extension = os.path.splitext(path)[1].lower()
    before = set(scene.objects.keys())
    if extension == ".blend":
        imported = sc_append_blend(scene, path, warnings)
    else:
        candidates = ASSET_IMPORTERS.get(extension)
        if candidates is None:
            raise RuntimeError("asset %s: %s is not an importable type" % (name, extension))
        imported = []
        for candidate in candidates:
            if sc_operator(candidate) is None:
                continue
            sc_guarded_op(scene, candidate, {"filepath": path}, warnings, name)
            break
        else:
            warnings.append("asset %s: no importer for %s in this Blender" % (name, extension))
        imported = [obj for obj in scene.objects if obj.name not in before]
    if extension == ".svg":
        imported = sc_svg_extrude(scene, imported, spec.get("extrude_depth", 0.05), warnings)
    if not imported:
        warnings.append("asset %s: import produced no objects" % name)
        return []
    material_name = spec.get("material")
    if material_name:
        material = bpy.data.materials.get(str(material_name))
        if material is None:
            warnings.append("asset %s: material %r is not defined" % (name, material_name))
        else:
            for obj in imported:
                sc_assign_material(obj, material)
    sc_normalize_import(imported, spec, warnings)
    return imported


def sc_world_bbox_of(objects):
    low = None
    high = None
    for obj in objects:
        if obj.type not in GEOMETRY_TYPES:
            continue
        box = world_bbox(obj)
        if not box:
            continue
        obj_low, obj_high = box
        low = list(obj_low) if low is None else [min(low[i], obj_low[i]) for i in range(3)]
        high = list(obj_high) if high is None else [max(high[i], obj_high[i]) for i in range(3)]
    return low, high


def sc_normalize_pivot(obj, warnings):
    """Moves an object's origin to its geometry centre without moving the geometry."""
    if obj.type != "MESH" or obj.data is None or obj.parent is not None:
        return False
    verts = obj.data.vertices
    if len(verts) == 0:
        return False
    low = [min(v.co[i] for v in verts) for i in range(3)]
    high = [max(v.co[i] for v in verts) for i in range(3)]
    centre = Vector([(low[i] + high[i]) * 0.5 for i in range(3)])
    if centre.length < 1e-9:
        return False
    obj.data.transform(Matrix.Translation(-centre))
    obj.data.update()
    obj.matrix_world = obj.matrix_world @ Matrix.Translation(centre)
    return True


def sc_normalize_import(objects, spec, warnings):
    """Centres, rescales and re-pivots imported geometry; material references are untouched."""
    name = spec.get("name") or "?"
    roots = [obj for obj in objects if obj.parent is None or obj.parent not in objects]
    if not roots:
        return
    low, high = sc_world_bbox_of(roots)
    if low is None:
        warnings.append("import %s: no measurable geometry to normalise" % name)
        return
    centre = Vector([(low[i] + high[i]) * 0.5 for i in range(3)])
    extent = [high[i] - low[i] for i in range(3)]
    longest = max(extent)
    fit = float(spec.get("fit_size", 0.0) or 0.0)
    if longest > 1000.0 or (longest > 0.0 and longest < 1e-3):
        warnings.append("import %s: extreme extent %.4g m normalised" % (name, longest))
    elif longest <= 1e-12:
        warnings.append("import %s: degenerate (zero-extent) geometry" % name)
        return
    scale = fit / longest if fit > 0.0 and longest > 1e-12 else 1.0
    target = Vector(tuple(float(v) for v in spec.get("target", (0.0, 0.0, 0.0))))
    anchor = target if spec.get("center", True) else centre
    transform = Matrix.Translation(anchor) @ Matrix.Scale(scale, 4) @ Matrix.Translation(-centre)
    for obj in roots:
        obj.matrix_world = transform @ obj.matrix_world
    bpy.context.view_layer.update()
    if spec.get("normalize_pivots", True):
        moved = sum(1 for obj in roots if sc_normalize_pivot(obj, warnings))
        if moved:
            bpy.context.view_layer.update()


def sc_missing_textures(objects, warnings):
    """Reports unassigned image nodes and texture files that cannot be found."""
    missing = []
    seen = set()
    for obj in objects:
        for slot in getattr(obj, "material_slots", ()):
            material = slot.material
            if material is None or not getattr(material, "use_nodes", False):
                continue
            if material.name in seen:
                continue
            seen.add(material.name)
            for node in material.node_tree.nodes:
                if node.type != "TEX_IMAGE":
                    continue
                image = node.image
                if image is None:
                    missing.append(
                        {"material": material.name, "texture": None, "reason": "image node has no image"}
                    )
                    continue
                if getattr(image, "packed_file", None) is not None:
                    continue
                path = ""
                try:
                    path = bpy.path.abspath(image.filepath) if image.filepath else ""
                except Exception:
                    path = ""
                if not path or not os.path.isfile(path):
                    missing.append(
                        {
                            "material": material.name,
                            "texture": image.name,
                            "path": image.filepath,
                            "reason": "texture file not found",
                        }
                    )
    for entry in missing:
        warnings.append(
            "missing texture in %s: %s (%s)"
            % (entry["material"], entry.get("texture") or "<unassigned>", entry["reason"])
        )
    return missing


# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------


def sc_geometry(scene):
    """Every geometry object in the scene, uncapped and name-sorted."""
    out = [obj for obj in scene.objects if obj.type in GEOMETRY_TYPES]
    out.sort(key=lambda obj: obj.name)
    return out


def sc_measure_object(obj, depsgraph):
    detail = {
        "name": obj.name,
        "kind": obj.type,
        "vertices": 0,
        "triangles": 0,
        "materials": [
            slot.material.name for slot in getattr(obj, "material_slots", ()) if slot.material
        ],
        "measured_from": "data",
    }
    if obj.type == "MESH" and obj.data is not None:
        detail["vertices"] = len(obj.data.vertices)
        obj.data.calc_loop_triangles()
        detail["triangles"] = len(obj.data.loop_triangles)
    elif obj.type in GEOMETRY_TYPES:
        try:
            evaluated = obj.evaluated_get(depsgraph)
            mesh = evaluated.to_mesh()
            if mesh is not None:
                mesh.calc_loop_triangles()
                detail["vertices"] = len(mesh.vertices)
                detail["triangles"] = len(mesh.loop_triangles)
                detail["measured_from"] = "evaluated"
                evaluated.to_mesh_clear()
        except Exception as exc:
            detail["measured_from"] = "unmeasured (%s)" % exc
    box = world_bbox(obj)
    if box:
        low, high = box
        detail["bbox_min"] = low
        detail["bbox_max"] = high
        detail["dimensions"] = [round(high[i] - low[i], 6) for i in range(3)]
    return detail


def sc_subject_bounds(scene):
    low, high = sc_world_bbox_of(sc_geometry(scene))
    if low is None:
        return [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]
    return low, high


def sc_midpoint(low, high):
    return [(low[i] + high[i]) * 0.5 for i in range(3)]


def sc_extent(low, high):
    return [high[i] - low[i] for i in range(3)]


# ---------------------------------------------------------------------------
# Cameras
# ---------------------------------------------------------------------------


def sc_new_camera(scene, name):
    data = bpy.data.cameras.new(name)
    obj = bpy.data.objects.new(name, data)
    scene.collection.objects.link(obj)
    scene.camera = obj
    return obj


def sc_look_at(location, target):
    direction = Vector(target) - Vector(location)
    if direction.length < 1e-9:
        return (0.0, 0.0, 0.0)
    return direction.to_track_quat("-Z", "Y").to_euler()


def sc_keyframe_location(obj, frame):
    obj.keyframe_insert(data_path="location", frame=frame)


def sc_aim_camera(scene, camera, target, name=None):
    """Aims a camera with a TRACK_TO constraint on an empty at the target.

    Keyframed euler angles are not a safe aim: to_euler() wraps at +/-180 degrees, and linear
    interpolation through the wrap swings the camera off the subject (measured: 1.9 m off axis
    on a 360 degree turntable). A constraint derives the rotation from the position instead.
    """
    empty = bpy.data.objects.new(name or (camera.name + "_TARGET"), None)
    empty.empty_display_type = "PLAIN_AXES"
    empty.empty_display_size = 0.25
    scene.collection.objects.link(empty)
    empty.location = Vector(target)
    constraint = camera.constraints.new(type="TRACK_TO")
    constraint.target = empty
    constraint.track_axis = "TRACK_NEGATIVE_Z"
    constraint.up_axis = "UP_Y"
    return empty


def sc_action_fcurves(action):
    """4.4+ still exposes action.fcurves, but layered actions keep them in channelbags."""
    curves = getattr(action, "fcurves", None)
    if curves is not None and len(curves):
        return list(curves)
    out = []
    for layer in getattr(action, "layers", ()):
        for strip in getattr(layer, "strips", ()):
            for bag in getattr(strip, "channelbags", ()):
                out.extend(bag.fcurves)
    return out


def sc_linear_timing(obj):
    animation = getattr(obj, "animation_data", None)
    action = getattr(animation, "action", None) if animation else None
    if action is None:
        return
    for curve in sc_action_fcurves(action):
        for point in curve.keyframe_points:
            point.interpolation = "LINEAR"


def sc_rig_target(rig, bounds):
    explicit = rig.get("target")
    if explicit:
        return [float(value) for value in explicit]
    return sc_midpoint(bounds[0], bounds[1])


def sc_orbit_camera(scene, rig, bounds, frame_start, frame_end):
    """A camera that arcs around the subject: constant radius, constant elevation."""
    target = sc_rig_target(rig, bounds)
    extent = sc_extent(bounds[0], bounds[1])
    radius = float(rig.get("radius") or 0.0)
    if radius <= 0.0:
        radius = max(2.5 * max(extent), 1.0)
    elevation = math.radians(float(rig.get("elevation_deg", 15.0)))
    start_angle = math.radians(float(rig.get("start_angle_deg", -35.0)))
    end_angle = math.radians(float(rig.get("end_angle_deg", 35.0)))
    cam = sc_new_camera(scene, rig.get("name") or "CINVAR_CAMERA")
    cam.data.lens = float(rig.get("focal_length_mm", 50.0))
    cam.data.sensor_width = 36.0
    fstop = rig.get("dof_fstop")
    if fstop:
        cam.data.dof.use_dof = True
        cam.data.dof.aperture_fstop = float(fstop)
    span = abs(end_angle - start_angle)
    steps = max(2, int(math.degrees(span) / 45.0) + 1)
    for index in range(steps + 1):
        fraction = index / float(steps)
        frame = frame_start + (frame_end - frame_start) * fraction
        angle = start_angle + (end_angle - start_angle) * fraction
        location = Vector(target) + Vector(
            (
                radius * math.cos(elevation) * math.sin(angle),
                -radius * math.cos(elevation) * math.cos(angle),
                radius * math.sin(elevation),
            )
        )
        cam.location = location
        if fstop:
            cam.data.dof.focus_distance = (Vector(target) - location).length
            cam.data.keyframe_insert(data_path="dof.focus_distance", frame=frame)
        sc_keyframe_location(cam, frame)
    sc_aim_camera(scene, cam, target)
    sc_linear_timing(cam)
    return cam, "orbit"


def sc_dolly_camera(scene, rig, bounds, frame_start, frame_end):
    """A straight push or pull between two camera positions, focused on the subject."""
    target = sc_rig_target(rig, bounds)
    extent = sc_extent(bounds[0], bounds[1])
    reach = max(max(extent) * 2.0, 1.0)
    start = Vector(tuple(float(v) for v in rig.get("start_pos", (-reach, -reach, reach * 0.6))))
    end = Vector(tuple(float(v) for v in rig.get("end_pos", (0.0, -reach * 1.4, reach * 0.5))))
    cam = sc_new_camera(scene, rig.get("name") or "CINVAR_CAMERA")
    cam.data.lens = float(rig.get("focal_length_mm", 50.0))
    cam.data.sensor_width = 36.0
    fstop = rig.get("dof_fstop")
    if fstop:
        cam.data.dof.use_dof = True
        cam.data.dof.aperture_fstop = float(fstop)
    for index in range(3):
        fraction = index / 2.0
        frame = frame_start + (frame_end - frame_start) * fraction
        location = start.lerp(end, fraction)
        cam.location = location
        if fstop:
            cam.data.dof.focus_distance = (Vector(target) - location).length
            cam.data.keyframe_insert(data_path="dof.focus_distance", frame=frame)
        sc_keyframe_location(cam, frame)
    sc_aim_camera(scene, cam, target)
    sc_linear_timing(cam)
    return cam, "dolly_track"


def sc_camera_basis(elevation, azimuth):
    """Camera right/up axes for an azimuth (0 = in front, -Y) and an elevation in radians."""
    forward = -Vector(
        (
            math.cos(elevation) * math.sin(azimuth),
            -math.cos(elevation) * math.cos(azimuth),
            math.sin(elevation),
        )
    )
    right = forward.cross(Vector((0.0, 0.0, 1.0)))
    if right.length < 1e-9:
        right = Vector((1.0, 0.0, 0.0))
    right.normalize()
    up = right.cross(forward)
    return right, up


def sc_ortho_scale(bounds, elevation, start_angle, turns, aspect, padding):
    """The smallest ortho_scale that keeps the whole bounding box in frame for every angle."""
    low, high = bounds
    centre = Vector(sc_midpoint(low, high))
    corners = [
        Vector((x, y, z)) - centre
        for x in (low[0], high[0])
        for y in (low[1], high[1])
        for z in (low[2], high[2])
    ]
    steps = max(12, int(12 * abs(turns)) + 1)
    widest = 1e-6
    tallest = 1e-6
    for index in range(steps + 1):
        azimuth = start_angle + 2.0 * math.pi * turns * (index / float(steps))
        right, up = sc_camera_basis(elevation, azimuth)
        for corner in corners:
            widest = max(widest, abs(corner.dot(right)))
            tallest = max(tallest, abs(corner.dot(up)))
    need_w = 2.0 * widest
    need_h = 2.0 * tallest
    scale = max(need_w, need_h * aspect) if aspect >= 1.0 else max(need_w / aspect, need_h)
    return max(scale, 1e-3) * (1.0 + max(padding, 0.0))


def sc_isometric_camera(scene, rig, bounds, render, frame_start, frame_end):
    """An orthographic turntable sized so the whole subject stays inside the frame."""
    target = sc_rig_target(rig, bounds)
    extent = sc_extent(bounds[0], bounds[1])
    elevation = math.radians(float(rig.get("elevation_deg", 35.264)))
    width = max(int(render.get("width", 1920)), 1)
    height = max(int(render.get("height", 1080)), 1)
    aspect = width / float(height)
    turns = float(rig.get("turns", 1.0))
    start_angle = math.radians(float(rig.get("start_angle_deg", 0.0)))
    padding = max(float(rig.get("padding", 0.15)), 0.0)
    ortho_scale = sc_ortho_scale(bounds, elevation, start_angle, turns, aspect, padding)
    distance = max(4.0 * max(extent), 2.0)
    cam = sc_new_camera(scene, rig.get("name") or "CINVAR_CAMERA")
    cam.data.type = "ORTHO"
    cam.data.ortho_scale = ortho_scale
    steps = max(4, int(4 * abs(turns)) + 4)
    for index in range(steps + 1):
        fraction = index / float(steps)
        frame = frame_start + (frame_end - frame_start) * fraction
        angle = start_angle + 2.0 * math.pi * turns * fraction
        location = Vector(target) + Vector(
            (
                distance * math.cos(elevation) * math.sin(angle),
                -distance * math.cos(elevation) * math.cos(angle),
                distance * math.sin(elevation),
            )
        )
        cam.location = location
        sc_keyframe_location(cam, frame)
    sc_aim_camera(scene, cam, target)
    sc_linear_timing(cam)
    return cam, "isometric_turntable"


def sc_build_camera(scene, rig, bounds, render, frame_start, frame_end):
    kind = str((rig or {}).get("rig", "orbit"))
    if kind == "dolly_track":
        return sc_dolly_camera(scene, rig, bounds, frame_start, frame_end)
    if kind == "isometric_turntable":
        return sc_isometric_camera(scene, rig, bounds, render, frame_start, frame_end)
    return sc_orbit_camera(scene, rig, bounds, frame_start, frame_end)


# ---------------------------------------------------------------------------
# Lighting
# ---------------------------------------------------------------------------


def sc_set_world(scene, color, strength):
    world = scene.world
    if world is None:
        world = bpy.data.worlds.new("CINVAR_WORLD")
        scene.world = world
    world.use_nodes = True
    tree = world.node_tree
    background = None
    output = None
    for node in tree.nodes:
        if node.type == "BACKGROUND":
            background = node
        elif node.type == "OUTPUT_WORLD":
            output = node
    if background is None:
        background = tree.nodes.new("ShaderNodeBackground")
        if output is not None:
            tree.links.new(background.outputs["Background"], output.inputs["Surface"])
    if "Color" in background.inputs:
        background.inputs["Color"].default_value = linear_rgba(color)
    if "Strength" in background.inputs:
        background.inputs["Strength"].default_value = max(float(strength), 0.0)


def sc_new_light(scene, name, kind, color, energy, location, aim, size=1.0, warnings=None):
    light_type = LIGHT_KINDS.get(str(kind).lower())
    if light_type is None:
        if warnings is not None:
            warnings.append("light %s: unknown type %r; using an area light" % (name, kind))
        light_type = "AREA"
    data = bpy.data.lights.new(name, type=light_type)
    data.energy = max(float(energy), 0.0)
    data.color = linear_rgb(color)
    if light_type == "AREA":
        data.size = max(float(size), 0.01)
        if hasattr(data, "shape"):
            try:
                data.shape = "SQUARE"
            except (TypeError, ValueError):
                pass
    if light_type == "SPOT" and hasattr(data, "spot_size"):
        data.spot_size = math.radians(60.0)
    obj = bpy.data.objects.new(name, data)
    scene.collection.objects.link(obj)
    obj.location = Vector(location)
    obj.rotation_euler = sc_look_at(location, aim)
    return obj


def sc_scaled_energy(intensity, radius):
    """Preset intensities are quoted at SCENE_REFERENCE_RADIUS; inverse square rescales them."""
    ratio = max(float(radius), 0.05) / SCENE_REFERENCE_RADIUS
    return float(intensity) * ratio * ratio


def sc_light_radius(bounds, explicit=None):
    if explicit:
        return max(float(explicit), 0.05)
    extent = sc_extent(bounds[0], bounds[1])
    return min(max(2.2 * max(extent), 0.75), 40.0)


def sc_studio_three_point(scene, preset, bounds, warnings):
    target = sc_midpoint(bounds[0], bounds[1])
    radius = sc_light_radius(bounds, preset.get("radius"))
    key_color = preset.get("key_color", "#fff3e4")
    key_intensity = float(preset.get("key_intensity", 900.0))
    fill_ratio = float(preset.get("fill_ratio", 0.35))
    rim_color = preset.get("rim_color", "#dde6ff")
    rim_ratio = float(preset.get("rim_ratio", 0.85))
    energy = sc_scaled_energy(key_intensity, radius)
    base = Vector(target)
    lights = [
        sc_new_light(scene, "CINVAR_KEY", "area", key_color, energy,
                     base + Vector((-0.75, -0.85, 0.95)) * radius, target, size=radius * 0.9),
        sc_new_light(scene, "CINVAR_FILL", "area", key_color, energy * fill_ratio,
                     base + Vector((0.95, -0.6, 0.3)) * radius, target, size=radius * 1.5),
        sc_new_light(scene, "CINVAR_RIM", "area", rim_color, energy * rim_ratio,
                     base + Vector((0.2, 1.0, 0.7)) * radius, target, size=radius * 0.7),
    ]
    sc_set_world(
        scene,
        preset.get("background", "#101216"),
        preset.get("background_strength", 0.25),
    )
    return lights


def sc_cyberpunk_neon(scene, preset, bounds, warnings):
    target = sc_midpoint(bounds[0], bounds[1])
    radius = sc_light_radius(bounds, preset.get("radius"))
    primary = preset.get("primary_color", "#ff2d95")
    secondary = preset.get("secondary_color", "#00e5ff")
    intensity = float(preset.get("intensity", 700.0))
    rim_ratio = float(preset.get("rim_ratio", 1.2))
    energy = sc_scaled_energy(intensity, radius)
    base = Vector(target)
    lights = [
        sc_new_light(scene, "CINVAR_NEON_PRIMARY", "area", primary, energy,
                     base + Vector((-0.95, -0.75, 0.55)) * radius, target, size=radius * 0.5),
        sc_new_light(scene, "CINVAR_NEON_SECONDARY", "area", secondary, energy * 0.8,
                     base + Vector((0.95, -0.55, 0.15)) * radius, target, size=radius * 0.5),
        sc_new_light(scene, "CINVAR_NEON_RIM", "area", secondary, energy * rim_ratio,
                     base + Vector((0.0, 1.05, 0.55)) * radius, target, size=radius * 0.6),
        sc_new_light(scene, "CINVAR_NEON_BOUNCE", "point", primary, energy * 0.35,
                     base + Vector((0.25, -0.35, -0.35)) * radius, target, size=radius * 0.2),
    ]
    sc_set_world(
        scene,
        preset.get("background", "#07030f"),
        preset.get("background_strength", 0.4),
    )
    return lights


def sc_product_cyclorama(scene, preset, bounds, warnings):
    target = sc_midpoint(bounds[0], bounds[1])
    radius = sc_light_radius(bounds, preset.get("radius"))
    sweep_color = preset.get("sweep_color", "#f2f3f5")
    backdrop_scale = float(preset.get("backdrop_scale", 6.0))
    floor_z = float(bounds[0][2])
    extent = max(sc_extent(bounds[0], bounds[1]))
    # The camera rig sits at about 4x the subject extent; the room has to be larger than that
    # so the camera (and every light) stays inside it whatever the turntable azimuth.
    span = max(extent, 0.05) * backdrop_scale
    verts, faces = sc_gen_cyclorama(
        floor_radius=span,
        curve_radius=span * 0.25,
        wall_height=span * 0.5,
    )
    sweep = sc_mesh_object(scene, "CINVAR_CYCLORAMA", verts, faces)
    sweep.location = Vector((target[0], target[1], floor_z))
    sc_face_towards(sweep, Vector((target[0], target[1], floor_z + max(extent * 0.5, 0.1))))
    sweep_material = sc_material(
        "CINVAR_SWEEP",
        {"base_color": sweep_color, "roughness": float(preset.get("sweep_roughness", 0.75)),
         "metallic": 0.0},
        PRINCIPLED_ALIASES,
        warnings,
    )
    sc_assign_material(sweep, sweep_material)
    key_intensity = float(preset.get("key_intensity", 90.0))
    fill_ratio = float(preset.get("fill_ratio", 0.5))
    energy = sc_scaled_energy(key_intensity, radius)
    base = Vector(target)
    lights = [
        sc_new_light(scene, "CINVAR_KEY", "area", preset.get("key_color", "#ffffff"), energy,
                     base + Vector((-0.45, -0.35, 1.55)) * radius, target, size=radius * 1.1),
        sc_new_light(scene, "CINVAR_FILL", "area", preset.get("fill_color", "#eef2ff"),
                     energy * fill_ratio, base + Vector((0.9, -0.75, 0.35)) * radius, target,
                     size=radius * 1.6),
        sc_new_light(scene, "CINVAR_RIM", "area", preset.get("rim_color", "#ffffff"),
                     energy * float(preset.get("rim_ratio", 0.7)),
                     base + Vector((0.15, 0.9, 1.1)) * radius, target, size=radius * 0.8),
    ]
    sc_set_world(
        scene,
        preset.get("background", "#d9dde3"),
        preset.get("background_strength", 0.3),
    )
    return lights


def sc_explicit_lights(scene, preset, bounds, warnings):
    target = sc_midpoint(bounds[0], bounds[1])
    radius = sc_light_radius(bounds, preset.get("radius"))
    lights = []
    for index, item in enumerate(preset.get("lights", [])):
        location = item.get("location")
        if not location:
            location = Vector(target) + Vector((-0.8, -0.8, 0.9)) * radius
        aim = item.get("target") or target
        energy = item.get("energy")
        if energy is None:
            energy = sc_scaled_energy(item.get("intensity", 900.0), radius)
        lights.append(
            sc_new_light(
                scene,
                item.get("name") or "CINVAR_LIGHT_%d" % (index + 1),
                item.get("type", "area"),
                item.get("color", "#ffffff"),
                energy,
                location,
                aim,
                size=item.get("size", radius * 0.8),
                warnings=warnings,
            )
        )
    if preset.get("background") is not None or preset.get("background_strength") is not None:
        sc_set_world(
            scene,
            preset.get("background", "#101216"),
            preset.get("background_strength", 0.25),
        )
    return lights


def sc_build_lighting(scene, lighting, bounds, warnings):
    preset = str((lighting or {}).get("preset", "studio_three_point"))
    if preset == "cyberpunk_neon":
        return sc_cyberpunk_neon(scene, lighting, bounds, warnings), preset
    if preset == "product_cyclorama":
        return sc_product_cyclorama(scene, lighting, bounds, warnings), preset
    if preset == "explicit":
        return sc_explicit_lights(scene, lighting, bounds, warnings), preset
    if preset != "studio_three_point":
        warnings.append("unknown lighting preset %r; using studio_three_point" % preset)
    return sc_studio_three_point(scene, lighting, bounds, warnings), "studio_three_point"


# ---------------------------------------------------------------------------
# Render settings and the three job modes
# ---------------------------------------------------------------------------


def sc_configure_render(scene, render, warnings):
    scene.render.engine = resolve_engine(scene, render.get("engine", "eevee"), warnings)
    scene.render.resolution_x = max(int(render.get("width", 1920)), 16)
    scene.render.resolution_y = max(int(render.get("height", 1080)), 16)
    scene.render.resolution_percentage = 100
    scene.render.fps = max(int(render.get("fps", 30)), 1)
    scene.render.fps_base = 1.0
    scene.frame_start = max(int(render.get("frame_start", 1)), 0)
    scene.frame_end = max(int(render.get("frame_end", 120)), int(scene.frame_start) + 1)
    samples = max(int(render.get("samples", 64)), 1)
    engine = scene.render.engine
    if engine == "CYCLES":
        cycles = getattr(scene, "cycles", None)
        for attribute, value in (("samples", samples), ("use_denoising", True)):
            if cycles is not None and hasattr(cycles, attribute):
                try:
                    setattr(cycles, attribute, value)
                except (TypeError, ValueError):
                    pass
    elif "EEVEE" in engine:
        eevee = getattr(scene, "eevee", None)
        if eevee is not None and hasattr(eevee, "taa_render_samples"):
            try:
                eevee.taa_render_samples = samples
            except (TypeError, ValueError):
                pass
    transparent = bool(render.get("transparent", False))
    scene.render.film_transparent = transparent
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA" if transparent else "RGB"
    scene.render.image_settings.color_depth = "8"
    # The colour the scene describes is the colour shown: AgX would desaturate it.
    try:
        scene.view_settings.view_transform = "Standard"
        scene.view_settings.look = "None"
    except (TypeError, ValueError) as exc:
        warnings.append("could not force the Standard view transform (%s)" % exc)
    scene.view_settings.exposure = 0.0
    scene.view_settings.gamma = 1.0
    if render.get("world_visible") is False:
        scene.render.film_transparent = True


def sc_build_object(scene, item, warnings):
    kind = item.get("kind") or {}
    label = str(kind.get("type", "cube"))
    name = str(item.get("name") or "CINVAR_OBJECT")
    if label == "text":
        obj = sc_text_object(scene, name, kind)
    elif label == "cube":
        obj = sc_mesh_object(scene, name, *sc_gen_cube(float(kind.get("size", 1.0))))
    elif label == "plane":
        obj = sc_mesh_object(scene, name, *sc_gen_plane(float(kind.get("size", 2.0))))
    elif label == "sphere":
        obj = sc_mesh_object(
            scene,
            name,
            *sc_gen_sphere(
                float(kind.get("radius", 0.5)),
                int(kind.get("segments", 32)),
                int(kind.get("rings", 16)),
            )
        )
    elif label == "cylinder":
        obj = sc_mesh_object(
            scene,
            name,
            *sc_gen_cylinder(
                float(kind.get("radius", 0.5)),
                float(kind.get("depth", 1.0)),
                int(kind.get("vertices", 32)),
            )
        )
    elif label == "torus":
        obj = sc_mesh_object(
            scene,
            name,
            *sc_gen_torus(
                float(kind.get("major_radius", 0.5)),
                float(kind.get("minor_radius", 0.15)),
                int(kind.get("major_segments", 48)),
                int(kind.get("minor_segments", 16)),
            )
        )
    else:
        warnings.append("object %s: unknown kind %r, built as a cube" % (name, label))
        obj = sc_mesh_object(scene, name, *sc_gen_cube(float(kind.get("size", 1.0))))
    sc_apply_transform(obj, item.get("transform"))
    return obj


def sc_vertex_total(objects):
    total = 0
    for obj in objects:
        if obj.type == "MESH" and obj.data is not None:
            total += len(obj.data.vertices)
    return total


def sc_build_scene(job):
    spec = job.get("scene") or {}
    out_dir = job["out_dir"]
    blend_path = job["blend"]
    aliases = job.get("socket_aliases") or PRINCIPLED_ALIASES
    warnings = []
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.name = str(spec.get("name") or "CINVAR_SCENE")

    for item in spec.get("materials", []):
        sc_material(str(item.get("name") or "CINVAR_MATERIAL"), item, aliases, warnings)

    asset_reports = []
    for item in spec.get("assets", []):
        imported = sc_import_asset(scene, item, warnings)
        bpy.context.view_layer.update()
        asset_reports.append(
            {
                "name": item.get("name"),
                "path": item.get("path"),
                "objects": sorted(obj.name for obj in imported),
                "vertices": sc_vertex_total(imported),
                "materials": sorted(
                    {
                        slot.material.name
                        for obj in imported
                        for slot in getattr(obj, "material_slots", ())
                        if slot.material
                    }
                ),
            }
        )

    for item in spec.get("objects", []):
        obj = sc_build_object(scene, item, warnings)
        material_name = item.get("material")
        if material_name:
            material = bpy.data.materials.get(str(material_name))
            if material is None:
                warnings.append("object %s: material %r is not defined" % (obj.name, material_name))
            else:
                sc_assign_material(obj, material)

    bpy.context.view_layer.update()
    bounds = sc_subject_bounds(scene)
    render = spec.get("render") or {}
    sc_configure_render(scene, render, warnings)
    frame_start = int(scene.frame_start)
    frame_end = int(scene.frame_end)
    camera, rig_kind = sc_build_camera(scene, spec.get("camera") or {}, bounds, render, frame_start, frame_end)
    lights, lighting_kind = sc_build_lighting(scene, spec.get("lighting") or {}, bounds, warnings)
    bpy.context.view_layer.update()

    depsgraph = bpy.context.evaluated_depsgraph_get()
    geometry = sc_geometry(scene)
    details = [sc_measure_object(obj, depsgraph) for obj in geometry]
    missing = sc_missing_textures(geometry, warnings)
    low, high = sc_world_bbox_of(geometry)
    light_objects = [obj for obj in scene.objects if obj.type == "LIGHT"]
    camera_objects = [obj for obj in scene.objects if obj.type == "CAMERA"]

    scene.frame_set(frame_start)
    bpy.context.view_layer.update()
    os.makedirs(os.path.dirname(blend_path) or ".", exist_ok=True)
    bpy.ops.wm.save_as_mainfile(filepath=blend_path, check_existing=False)

    report = {
        "blend": blend_path,
        "out_dir": out_dir,
        "scene_name": scene.name,
        "objects": len(geometry),
        "cameras": len(camera_objects),
        "lights": len(light_objects),
        "vertices": sum(detail["vertices"] for detail in details),
        "object_details": details,
        "camera": camera.name,
        "camera_rig": rig_kind,
        "camera_objects": sorted(obj.name for obj in camera_objects),
        "lighting": lighting_kind,
        "light_names": sorted(obj.name for obj in light_objects),
        "material_count": len(bpy.data.materials),
        "materials": sorted(bpy.data.materials.keys()),
        "missing_textures": missing,
        "assets": asset_reports,
        "engine": scene.render.engine,
        "resolution": [int(scene.render.resolution_x), int(scene.render.resolution_y)],
        "fps": int(scene.render.fps),
        "frame_start": frame_start,
        "frame_end": frame_end,
        "transparent": bool(scene.render.film_transparent),
        "bbox_min": low,
        "bbox_max": high,
        "blender_version": blender_version_text(),
        "socket_alias_source": "job" if job.get("socket_aliases") else "sdk",
        "warnings": warnings,
    }
    write_json(os.path.join(out_dir, "report.json"), report)
    log(
        "scene built: objects %d cameras %d lights %d vertices %d -> %s"
        % (report["objects"], report["cameras"], report["lights"], report["vertices"], blend_path)
    )
    return 0


def sc_verify_scene(job):
    out_dir = job["out_dir"]
    scene = bpy.context.scene
    depsgraph = bpy.context.evaluated_depsgraph_get()
    geometry = sc_geometry(scene)
    details = [sc_measure_object(obj, depsgraph) for obj in geometry]
    low, high = sc_world_bbox_of(geometry)
    report = {
        "file": bpy.data.filepath,
        "objects": len(geometry),
        "cameras": len([obj for obj in scene.objects if obj.type == "CAMERA"]),
        "lights": len([obj for obj in scene.objects if obj.type == "LIGHT"]),
        "vertices": sum(detail["vertices"] for detail in details),
        "object_details": details,
        "material_count": len(bpy.data.materials),
        "engine": scene.render.engine,
        "resolution": [int(scene.render.resolution_x), int(scene.render.resolution_y)],
        "fps": int(scene.render.fps),
        "frame_start": int(scene.frame_start),
        "frame_end": int(scene.frame_end),
        "transparent": bool(scene.render.film_transparent),
        "bbox_min": low,
        "bbox_max": high,
        "blender_version": blender_version_text(),
        "camera_animated": bool(
            scene.camera is not None
            and getattr(scene.camera, "animation_data", None) is not None
            and scene.camera.animation_data.action is not None
        ),
        "warnings": [],
    }
    write_json(os.path.join(out_dir, "verify.json"), report)
    log(
        "scene verified from file: objects %d cameras %d lights %d"
        % (report["objects"], report["cameras"], report["lights"])
    )
    return 0


def sc_render_still(job):
    scene = bpy.context.scene
    out_dir = job["out_dir"]
    target = job["still"]
    frame = job.get("frame")
    if frame is None:
        frame = int((int(scene.frame_start) + int(scene.frame_end)) // 2)
    frame = int(frame)
    scene.frame_set(frame)
    bpy.context.view_layer.update()
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGB"
    scene.render.resolution_percentage = 100
    scene.render.use_file_extension = True
    warnings = []
    attempts = []
    requested = scene.render.engine
    engines = [requested]
    if requested != "CYCLES":
        engines.append("CYCLES")
    used = None
    for engine in engines:
        available = engine_ids(scene)
        if available and engine not in available:
            attempts.append({"engine": engine, "ok": False, "detail": "engine not offered"})
            continue
        try:
            scene.render.engine = engine
        except (TypeError, ValueError) as exc:
            attempts.append({"engine": engine, "ok": False, "detail": str(exc)})
            continue
        if engine == "CYCLES" and hasattr(scene, "cycles"):
            try:
                scene.cycles.samples = int(job.get("fallback_samples", 48))
            except (TypeError, ValueError):
                pass
        scene.render.filepath = target
        if os.path.isfile(target):
            os.remove(target)
        try:
            bpy.ops.render.render(write_still=True)
        except Exception as exc:
            warnings.append("render with %s failed (%s)" % (engine, exc))
            attempts.append({"engine": engine, "ok": False, "detail": str(exc)})
            continue
        if os.path.isfile(target) and os.path.getsize(target) > 0:
            used = engine
            attempts.append({"engine": engine, "ok": True, "bytes": os.path.getsize(target)})
            break
        attempts.append({"engine": engine, "ok": False, "detail": "no file written"})
    if used is None:
        raise RuntimeError("no engine produced a still: %s" % warnings)
    write_json(
        os.path.join(out_dir, "still.json"),
        {
            "still": target,
            "frame": frame,
            "engine": used,
            "requested_engine": requested,
            "attempts": attempts,
            "resolution": [int(scene.render.resolution_x), int(scene.render.resolution_y)],
            "transparent": bool(scene.render.film_transparent),
            "blender_version": blender_version_text(),
            "warnings": warnings,
        },
    )
    log("still rendered at frame %d with %s -> %s" % (frame, used, target))
    return 0


SCENE_MODES = {
    SCENE_MODE_BUILD: sc_build_scene,
    SCENE_MODE_VERIFY: sc_verify_scene,
    SCENE_MODE_STILL: sc_render_still,
}


def run_scene(job):
    """Entry point for a declarative scene job: build, verify, or render one still."""
    try:
        mode = str(job.get("mode", SCENE_MODE_BUILD))
        handler = SCENE_MODES.get(mode)
        if handler is None:
            raise RuntimeError("unknown scene mode %r" % mode)
        handler(job)
    except Exception:
        detail = traceback.format_exc()
        log("SCENE FAILED\n" + detail)
        try:
            write_json(
                os.path.join(job.get("out_dir", "."), "error.json"),
                {"error": detail.strip().splitlines()[-1], "traceback": detail, "mode": job.get("mode")},
            )
        except Exception:
            pass
        raise

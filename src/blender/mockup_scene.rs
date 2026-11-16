//! Python scene driver for the mockup module (Argon 5D).
//!
//! 'cinvar.blender.mockup' places a real video on a 3D surface. The geometry, lights, camera
//! path and the image-sequence texture are built by the driver embedded here: the caller only
//! supplies a 'MockupJob' (a JSON file next to the driver), never python. Keeping the driver
//! a single versioned constant means a render request is reproducible from its job file, and
//! the script can be syntax-checked without Blender installed.
//!
//! Split out of 'mockup.rs' so the python generation stays reviewable in one place.

use serde::Serialize;

/// Bump when the generated scene changes what a frame looks like: it is part of the job hash,
/// so a cached job directory is never resumed with frames from an older scene.
pub const SCRIPT_VERSION: u32 = 1;

/// Everything the driver needs. Serialized to 'mockup_job.json' in the job directory.
#[derive(Debug, Clone, Serialize)]
pub struct MockupJob {
    /// floating_screen | glass_browser | laptop | phone
    pub kind: String,
    /// static | orbit | dolly_in | pan_right | handheld
    pub camera: String,
    pub camera_strength: f64,
    pub lighting: String,
    pub light_strength: f64,
    pub reflections: f64,
    /// 0.0 disables depth of field.
    pub f_stop: f64,
    /// Shutter in frames; 0.0 disables motion blur.
    pub motion_blur: f64,
    /// cycles | eevee
    pub engine: String,
    pub samples: u32,
    pub denoise: bool,
    pub transparent: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frame_start: u32,
    pub frame_end: u32,
    /// How many texture frames were extracted from the source clip.
    pub tex_frames: u32,
    /// Source display aspect (width / height).
    pub aspect: f64,
    pub seed: u32,
    /// Height of the rig (screen centre) above the floor.
    pub rig_z: f64,
    pub tilt_x_deg: f64,
    pub tilt_z_deg: f64,
    /// Accent colour in linear RGB.
    pub accent: [f64; 3],
    /// Nominal display box (metres) the video is fitted into.
    pub nominal_display: [f64; 2],
}

impl MockupJob {
    pub fn frame_count(&self) -> u32 {
        self.frame_end.saturating_sub(self.frame_start) + 1
    }
}

/// The driver script written into the job directory next to 'mockup_job.json'.
pub fn script_for(job: &MockupJob) -> String {
    let header = format!(
        "# CINVAR Blender mockup driver -- generated automatically, edits are overwritten.\n\
         # scene: {}  engine: {}  samples: {}  frames: {}..{}  texture frames: {}\n\
         # script version: {}\n",
        job.kind,
        job.engine,
        job.samples,
        job.frame_start,
        job.frame_end,
        job.tex_frames,
        SCRIPT_VERSION
    );
    let mut out = String::with_capacity(DRIVER.len() + header.len());
    out.push_str(&header);
    out.push_str(DRIVER);
    out
}

/// The driver itself. It reads 'mockup_job.json' from its own directory, builds the scene,
/// measures the projected screen rectangle for every frame, then renders the PNG sequence.
const DRIVER: &str = r##"
import json
import math
import os

import bpy
from mathutils import Vector

HERE = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(HERE, "mockup_job.json"), "r") as fh:
    CFG = json.load(fh)

FRAMES_DIR = os.path.join(HERE, "frames")
TEX_DIR = os.path.join(HERE, "tex")
MEASURE_PATH = os.path.join(HERE, "mockup_measure.json")
FRAME_START = int(CFG["frame_start"])
FRAME_END = int(CFG["frame_end"])
TEX_FRAMES = int(CFG["tex_frames"])
AR = float(CFG["aspect"])
WIDTH = int(CFG["width"])
HEIGHT = int(CFG["height"])
FPS = int(CFG["fps"])
TRANSPARENT = bool(CFG["transparent"])
RIG_Z = float(CFG["rig_z"])
FILL = 0.80
LENS = 50.0
SENSOR = 36.0
# A default plane lies in XY facing +Z; screen surfaces must stand up facing the camera (-Y).
VERTICAL = (math.pi * 0.5, 0.0, 0.0)


def log(msg):
    print("[cinvar-mockup] %s" % msg, flush=True)


def png_complete(path):
    try:
        with open(path, "rb") as fh:
            if fh.read(8) != b"\x89PNG\r\n\x1a\n":
                return False
            fh.seek(-12, os.SEEK_END)
            return fh.read(12)[4:8] == b"IEND"
    except OSError:
        return False


# ---- settings shims (Blender 3.6 .. 4.x renamed the Principled inputs) --------
def set_input(node, names, value):
    if isinstance(names, str):
        names = (names,)
    for name in names:
        sock = node.inputs.get(name)
        if sock is not None:
            sock.default_value = value
            return True
    return False


def set_attr(obj, name, value):
    if hasattr(obj, name):
        try:
            setattr(obj, name, value)
            return True
        except (AttributeError, TypeError, ValueError):
            return False
    return False


def reset_scene():
    for ob in list(bpy.data.objects):
        bpy.data.objects.remove(ob, do_unlink=True)
    for block in (bpy.data.meshes, bpy.data.materials, bpy.data.lights, bpy.data.cameras):
        for item in list(block):
            if item.users == 0:
                block.remove(item)


# ---- materials -----------------------------------------------------------------
def new_material(name):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    for node in list(nt.nodes):
        nt.nodes.remove(node)
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    out.location = (420.0, 0.0)
    return mat, nt, out


def principled(name, base, metallic=0.0, roughness=0.4, emission=None,
               emission_strength=0.0, transmission=0.0, ior=1.45, coat=0.0):
    mat, nt, out = new_material(name)
    bsdf = nt.nodes.new("ShaderNodeBsdfPrincipled")
    set_input(bsdf, "Base Color", (base[0], base[1], base[2], 1.0))
    set_input(bsdf, "Metallic", metallic)
    set_input(bsdf, "Roughness", roughness)
    set_input(bsdf, "IOR", ior)
    if transmission > 0.0:
        set_input(bsdf, ("Transmission Weight", "Transmission"), transmission)
    if coat > 0.0:
        set_input(bsdf, ("Coat Weight", "Clearcoat"), coat)
        set_input(bsdf, ("Coat Roughness", "Clearcoat Roughness"), 0.03)
    if emission is not None:
        set_input(bsdf, ("Emission Color", "Emission"), (emission[0], emission[1], emission[2], 1.0))
        set_input(bsdf, "Emission Strength", emission_strength)
    nt.links.new(bsdf.outputs["BSDF"], out.inputs["Surface"])
    return mat


def glass_sheet(name, alpha, roughness):
    mat, nt, out = new_material(name)
    bsdf = nt.nodes.new("ShaderNodeBsdfPrincipled")
    # Real glass has no diffuse: a white base would lay a lit haze over the screen behind it.
    set_input(bsdf, "Base Color", (0.015, 0.018, 0.024, 1.0))
    set_input(bsdf, "Metallic", 0.0)
    set_input(bsdf, "Roughness", roughness)
    set_input(bsdf, "IOR", 1.45)
    set_input(bsdf, ("Specular IOR Level", "Specular"), 0.55)
    set_input(bsdf, "Alpha", alpha)
    set_attr(mat, "blend_method", "BLEND")
    set_attr(mat, "shadow_method", "HASHED")
    set_attr(mat, "use_screen_refraction", False)
    nt.links.new(bsdf.outputs["BSDF"], out.inputs["Surface"])
    return mat


def screen_material(name, roughness, specular):
    """The mapped video surface: the image sequence drives a Principled emission layer, so the
    clip appears on the surface at its own colours and the surface still takes reflections."""
    mat, nt, out = new_material(name)
    shade = nt.nodes.new("ShaderNodeBsdfPrincipled")
    set_input(shade, "Base Color", (0.01, 0.01, 0.012, 1.0))
    set_input(shade, "Roughness", roughness)
    set_input(shade, "Metallic", 0.0)
    set_input(shade, ("Specular IOR Level", "Specular"), specular)
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.name = "CINVAR_VIDEO"
    first = os.path.join(TEX_DIR, "frame_%04d.png" % FRAME_START)
    if not os.path.isfile(first):
        raise SystemExit("mockup: texture frame missing: %s" % first)
    img = bpy.data.images.load(first, check_existing=False)
    img.source = "SEQUENCE"
    img.reload()
    img.colorspace_settings.name = "sRGB"
    tex.image = img
    tex.interpolation = "Linear"
    tex.extension = "EXTEND"
    tex.image_user.frame_duration = TEX_FRAMES
    tex.image_user.frame_start = FRAME_START
    tex.image_user.frame_offset = 0
    tex.image_user.use_auto_refresh = True
    set_input(shade, ("Emission Color", "Emission"), (1.0, 1.0, 1.0, 1.0))
    emission_socket = "Emission Color" if "Emission Color" in shade.inputs else "Emission"
    nt.links.new(tex.outputs["Color"], shade.inputs[emission_socket])
    set_input(shade, "Emission Strength", 1.0)
    nt.links.new(shade.outputs["BSDF"], out.inputs["Surface"])
    log("texture: %s frames=%d detected_duration=%d" % (first, TEX_FRAMES, img.frame_duration))
    return mat


def build_materials():
    refl = float(CFG["reflections"])
    accent = CFG["accent"]
    mats = {}
    mats["screen"] = screen_material("CINVAR_SCREEN_MAT", 0.30 - 0.26 * refl, 0.30 + 0.50 * refl)
    mats["screen_off"] = principled("CINVAR_DISPLAY_OFF", (0.004, 0.004, 0.005), roughness=0.22)
    mats["metal"] = principled("CINVAR_METAL", (0.42, 0.44, 0.48), metallic=1.0, roughness=0.28)
    mats["metal_dark"] = principled("CINVAR_METAL_DARK", (0.055, 0.058, 0.066), metallic=0.85, roughness=0.32)
    mats["carbon"] = principled("CINVAR_CARBON", (0.020, 0.021, 0.024), metallic=0.2, roughness=0.42)
    mats["keys"] = principled("CINVAR_KEYS", (0.030, 0.031, 0.034), metallic=0.1, roughness=0.55)
    # A glass pane is a sheen, not a lens: a thin sheet with alpha plus a sharp specular lobe.
    # A solid refracting slab in front of an emissive screen is the noisiest surface in this
    # scene and turns a low-sample Cycles render into speckle.
    # Roughness here is the anti-firefly knob: a sharper lobe reflects the area lights as sparse
    # speckle, a broader one spreads them into the soft sheen a real pane reads as.
    mats["glass"] = glass_sheet("CINVAR_GLASS", 0.10 + 0.08 * (1.0 - refl), 0.38 - 0.14 * refl)
    mats["accent"] = principled("CINVAR_ACCENT", accent, roughness=0.35,
                                emission=accent, emission_strength=5.0)
    mats["pill"] = principled("CINVAR_PILL", (0.35, 0.36, 0.40), roughness=0.30,
                              emission=(0.65, 0.68, 0.75), emission_strength=0.35)
    mats["dot_r"] = principled("CINVAR_DOT_R", (0.55, 0.05, 0.05), roughness=0.2,
                               emission=(0.95, 0.16, 0.12), emission_strength=2.4)
    mats["dot_y"] = principled("CINVAR_DOT_Y", (0.55, 0.35, 0.02), roughness=0.2,
                               emission=(0.98, 0.68, 0.10), emission_strength=2.4)
    mats["dot_g"] = principled("CINVAR_DOT_G", (0.05, 0.45, 0.10), roughness=0.2,
                               emission=(0.22, 0.85, 0.32), emission_strength=2.4)
    mats["floor"] = principled("CINVAR_FLOOR", (0.030, 0.031, 0.035), roughness=0.55 - 0.44 * refl)
    mats["backdrop"] = principled("CINVAR_BACKDROP", (0.014, 0.015, 0.019), roughness=0.65)
    return mats


# ---- geometry ------------------------------------------------------------------
def finish(ob, mat):
    ob["cinvar_part"] = True
    if mat is not None:
        ob.data.materials.append(mat)
    return ob


def parent_to(ob, parent):
    if parent is None:
        return ob
    from mathutils import Matrix
    ob.parent = parent
    ob.matrix_parent_inverse = Matrix.Identity(4)
    return ob


def add_cube(name, size, loc=(0.0, 0.0, 0.0), rot=(0.0, 0.0, 0.0), bevel=0.0,
             segments=3, mat=None, parent=None):
    bpy.ops.mesh.primitive_cube_add(size=1.0, location=(0.0, 0.0, 0.0))
    ob = finish(bpy.context.object, mat)
    ob.name = name
    ob.scale = (size[0], size[1], size[2])
    bpy.context.view_layer.objects.active = ob
    bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
    width = min(bevel, 0.45 * min(size))
    if width > 0.0:
        mod = ob.modifiers.new("bevel", "BEVEL")
        mod.width = width
        mod.segments = segments
        mod.limit_method = "ANGLE"
        mod.angle_limit = math.radians(35.0)
        mod.harden_normals = False
    ob.location = loc
    ob.rotation_euler = rot
    return parent_to(ob, parent)


def add_plane(name, w, h, loc=(0.0, 0.0, 0.0), rot=(0.0, 0.0, 0.0), mat=None, parent=None):
    bpy.ops.mesh.primitive_plane_add(size=2.0, location=(0.0, 0.0, 0.0))
    ob = finish(bpy.context.object, mat)
    ob.name = name
    ob.scale = (w * 0.5, h * 0.5, 1.0)
    ob.location = loc
    ob.rotation_euler = rot
    return parent_to(ob, parent)


def add_sphere(name, radius, loc=(0.0, 0.0, 0.0), mat=None, parent=None):
    bpy.ops.mesh.primitive_uv_sphere_add(radius=radius, segments=16, ring_count=10,
                                         location=(0.0, 0.0, 0.0))
    ob = finish(bpy.context.object, mat)
    ob.name = name
    ob.location = loc
    return parent_to(ob, parent)


def add_cylinder(name, radius, length, loc=(0.0, 0.0, 0.0), mat=None, parent=None):
    bpy.ops.mesh.primitive_cylinder_add(radius=radius, depth=length, vertices=24,
                                        rotation=(0.0, math.radians(90.0), 0.0),
                                        location=(0.0, 0.0, 0.0))
    ob = finish(bpy.context.object, mat)
    ob.name = name
    ob.location = loc
    return parent_to(ob, parent)


def contain(box_w, box_h, aspect):
    """Largest w x h with the given aspect that fits in the box (letterbox, never stretch)."""
    if aspect >= box_w / box_h:
        return (box_w, box_w / aspect)
    return (box_h * aspect, box_h)


def add_display(name, box_w, box_h, z=0.0, parent=None, mats=None, off=0.0):
    """Black display panel plus the video quad fitted inside it (1% inset)."""
    add_plane(name + "_panel", box_w, box_h, loc=(0.0, -off, z), rot=VERTICAL,
              mat=mats["screen_off"], parent=parent)
    vw, vh = contain(box_w * 0.99, box_h * 0.99, AR)
    quad = add_plane(name, vw, vh, loc=(0.0, -off - 0.002, z), rot=VERTICAL,
                     mat=mats["screen"], parent=parent)
    return quad


# ---- scene kinds: built with the screen centre at the rig origin ----------------
def device_floating(rig, mats):
    box_w, box_h = CFG["nominal_display"]
    quad = add_display("CINVAR_SCREEN", box_w, box_h, z=0.0, parent=rig, mats=mats)
    add_cube("bezel", (box_w + 0.14, 0.075, box_h + 0.14), loc=(0.0, 0.052, 0.0),
             bevel=0.030, segments=4, mat=mats["metal_dark"], parent=rig)
    add_cube("accent_strip", (box_w * 0.30, 0.010, 0.016),
             loc=(0.0, 0.013, -(box_h * 0.5 + 0.045)), bevel=0.004,
             mat=mats["accent"], parent=rig)
    return quad


def device_glass(rig, mats):
    box_w, box_h = CFG["nominal_display"]
    add_cube("card", (box_w + 0.40, 0.055, box_h + 0.62), loc=(0.0, 0.040, 0.10),
             bevel=0.045, segments=4, mat=mats["carbon"], parent=rig)
    quad = add_display("CINVAR_SCREEN", box_w, box_h, z=0.02, parent=rig, mats=mats)
    bar_z = box_h * 0.5 + 0.12
    add_cube("chrome", (box_w + 0.40, 0.070, 0.24), loc=(0.0, 0.010, bar_z),
             bevel=0.020, segments=3, mat=mats["metal_dark"], parent=rig)
    for index, key in enumerate(("dot_r", "dot_y", "dot_g")):
        add_sphere("dot%d" % index, 0.030,
                   loc=(-box_w * 0.5 - 0.06 + index * 0.105, -0.030, bar_z),
                   mat=mats[key], parent=rig)
    add_cube("url_pill", (box_w * 0.46, 0.014, 0.095), loc=(0.0, -0.032, bar_z),
             bevel=0.045, segments=4, mat=mats["pill"], parent=rig)
    add_plane("glass_front", box_w + 0.46, box_h + 0.68, loc=(0.0, -0.080, 0.08),
              rot=VERTICAL, mat=mats["glass"], parent=rig)
    return quad


def device_laptop(rig, mats):
    box_w, box_h = CFG["nominal_display"]
    lid_w = box_w + 0.34
    lid_h = box_h + 0.32
    hinge_y = 0.020
    hinge_z = -(box_h * 0.5 + 0.17)
    from mathutils import Matrix
    lid = bpy.data.objects.new("lid_pivot", None)
    bpy.context.scene.collection.objects.link(lid)
    lid.parent = rig
    lid.matrix_parent_inverse = Matrix.Identity(4)
    lid.location = (0.0, hinge_y, hinge_z)
    lid.rotation_euler = (math.radians(-12.0), 0.0, 0.0)
    add_cube("lid", (lid_w, 0.052, lid_h), loc=(0.0, 0.0, lid_h * 0.5 - 0.02),
             bevel=0.026, segments=3, mat=mats["metal"], parent=lid)
    inner = contain(lid_w - 0.30, lid_h - 0.30, AR)
    add_plane("screen_panel", lid_w - 0.28, lid_h - 0.28,
              loc=(0.0, -0.0275, lid_h * 0.5 - 0.02), rot=VERTICAL,
              mat=mats["screen_off"], parent=lid)
    quad = add_plane("CINVAR_SCREEN", inner[0], inner[1],
                     loc=(0.0, -0.030, lid_h * 0.5 - 0.02), rot=VERTICAL,
                     mat=mats["screen"], parent=lid)
    deck_len = 1.78
    deck_z = hinge_z - 0.048
    add_cube("deck", (lid_w, deck_len, 0.096), loc=(0.0, hinge_y - deck_len * 0.5, deck_z),
             bevel=0.030, segments=3, mat=mats["metal"], parent=rig)
    add_cube("keybed", (lid_w - 0.44, deck_len * 0.52, 0.012),
             loc=(0.0, hinge_y - deck_len * 0.30, deck_z + 0.054),
             bevel=0.010, segments=2, mat=mats["carbon"], parent=rig)
    cols, rows = 14, 5
    key_w = (lid_w - 0.62) / cols
    for row in range(rows):
        for col in range(cols):
            x = -(lid_w - 0.62) * 0.5 + key_w * (col + 0.5)
            y = hinge_y - deck_len * 0.30 + (rows * 0.5 - row - 0.5) * 0.115
            add_cube("key_%d_%d" % (row, col), (key_w * 0.86, 0.100, 0.014),
                     loc=(x, y, deck_z + 0.064), bevel=0.006, segments=2,
                     mat=mats["keys"], parent=rig)
    add_cube("trackpad", (0.86, 0.50, 0.008),
             loc=(0.0, hinge_y - deck_len * 0.80, deck_z + 0.052),
             bevel=0.010, segments=3, mat=mats["keys"], parent=rig)
    add_cylinder("hinge", 0.045, lid_w - 0.10, loc=(0.0, hinge_y, hinge_z + 0.01),
                 mat=mats["metal_dark"], parent=rig)
    return quad


def device_phone(rig, mats):
    body_w, body_h = 0.98, 1.98
    add_cube("body", (body_w, 0.062, body_h), bevel=0.095, segments=8,
             mat=mats["metal_dark"], parent=rig)
    box_w, box_h = CFG["nominal_display"]
    quad = add_display("CINVAR_SCREEN", box_w, box_h, z=0.0, parent=rig, mats=mats, off=0.034)
    add_cube("notch", (0.20, 0.010, 0.032), loc=(0.0, -0.037, box_h * 0.5 - 0.012),
             bevel=0.016, segments=4, mat=mats["carbon"], parent=rig)
    add_cube("button_power", (0.012, 0.040, 0.22), loc=(body_w * 0.5, 0.0, 0.28),
             bevel=0.006, segments=2, mat=mats["metal"], parent=rig)
    add_cube("button_vol", (0.012, 0.040, 0.30), loc=(-body_w * 0.5, 0.0, 0.34),
             bevel=0.006, segments=2, mat=mats["metal"], parent=rig)
    return quad


# ---- environment and lights -----------------------------------------------------
def world_bounds(objects):
    lo = Vector((1e9, 1e9, 1e9))
    hi = Vector((-1e9, -1e9, -1e9))
    for ob in objects:
        for corner in ob.bound_box:
            point = ob.matrix_world @ Vector(corner)
            for axis in range(3):
                lo[axis] = min(lo[axis], point[axis])
                hi[axis] = max(hi[axis], point[axis])
    return lo, hi


def build_environment(scene, bounds, mats):
    lo, hi = bounds
    radius = max(hi.x - lo.x, hi.y - lo.y, hi.z - lo.z)
    ground = lo.z - 0.02
    if CFG["kind"] in ("floating_screen", "glass_browser"):
        ground = lo.z - 0.55
    floor = add_plane("floor", 44.0, 44.0, loc=(0.0, -2.0, ground), mat=mats["floor"])
    backdrop = add_plane("backdrop", 42.0, 26.0,
                         loc=(0.0, 8.0, ground + 9.0),
                         rot=(math.radians(90.0), 0.0, 0.0), mat=mats["backdrop"])
    floor["cinvar_part"] = False
    backdrop["cinvar_part"] = False
    if TRANSPARENT:
        bpy.data.objects.remove(floor, do_unlink=True)
        bpy.data.objects.remove(backdrop, do_unlink=True)
    world = bpy.data.worlds.new("CINVAR_WORLD")
    scene.world = world
    world.use_nodes = True
    nt = world.node_tree
    for node in list(nt.nodes):
        nt.nodes.remove(node)
    out = nt.nodes.new("ShaderNodeOutputWorld")
    bg = nt.nodes.new("ShaderNodeBackground")
    strength = {"studio": 0.22, "softbox": 0.34, "neon": 0.10, "daylight": 0.55}[CFG["lighting"]]
    tint = {"studio": (0.055, 0.060, 0.075), "softbox": (0.075, 0.078, 0.085),
            "neon": (0.045, 0.020, 0.060), "daylight": (0.10, 0.105, 0.115)}[CFG["lighting"]]
    bg.inputs["Color"].default_value = (tint[0], tint[1], tint[2], 1.0)
    bg.inputs["Strength"].default_value = strength
    nt.links.new(bg.outputs["Background"], out.inputs["Surface"])
    return (lo, hi), radius


def aim(ob, target):
    direction = Vector(target) - ob.location
    ob.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler()


def area_light(name, location, target, size, energy, color):
    data = bpy.data.lights.new(name, "AREA")
    data.shape = "RECTANGLE"
    data.size = size
    data.size_y = size * 0.62
    data.energy = energy
    data.color = color
    ob = bpy.data.objects.new(name, data)
    bpy.context.scene.collection.objects.link(ob)
    ob.location = location
    aim(ob, target)
    return ob


def build_lights(bounds, radius):
    lo, hi = bounds
    center = Vector(((lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5, (lo.z + hi.z) * 0.5))
    unit = max(radius, 0.6)
    power = unit * unit * float(CFG["light_strength"])
    preset = CFG["lighting"]
    made = []
    if preset == "neon":
        made.append(area_light("key", center + Vector((-1.7 * unit, -1.9 * unit, 1.3 * unit)),
                               center, 2.4 * unit, 240.0 * power, (1.0, 0.16, 0.62)))
        made.append(area_light("rim", center + Vector((1.6 * unit, 1.7 * unit, 1.3 * unit)),
                               center, 2.0 * unit, 300.0 * power, (0.20, 0.80, 1.0)))
        made.append(area_light("fill", center + Vector((1.9 * unit, -1.4 * unit, 0.2 * unit)),
                               center, 3.0 * unit, 60.0 * power, (0.30, 0.35, 0.75)))
    elif preset == "daylight":
        made.append(area_light("key", center + Vector((-1.2 * unit, -1.7 * unit, 2.0 * unit)),
                               center, 3.4 * unit, 420.0 * power, (1.0, 0.98, 0.95)))
        made.append(area_light("fill", center + Vector((1.7 * unit, -1.5 * unit, 0.9 * unit)),
                               center, 3.6 * unit, 190.0 * power, (0.92, 0.96, 1.0)))
        made.append(area_light("rim", center + Vector((0.4 * unit, 2.0 * unit, 1.6 * unit)),
                               center, 2.2 * unit, 220.0 * power, (1.0, 1.0, 1.0)))
    elif preset == "softbox":
        made.append(area_light("key", center + Vector((-1.5 * unit, -1.7 * unit, 1.0 * unit)),
                               center, 3.6 * unit, 300.0 * power, (1.0, 0.97, 0.93)))
        made.append(area_light("fill", center + Vector((1.5 * unit, -1.7 * unit, 1.0 * unit)),
                               center, 3.6 * unit, 300.0 * power, (0.96, 0.97, 1.0)))
        made.append(area_light("rim", center + Vector((0.0, 1.8 * unit, 1.4 * unit)),
                               center, 2.4 * unit, 200.0 * power, (1.0, 1.0, 1.0)))
    else:  # studio
        made.append(area_light("key", center + Vector((-1.6 * unit, -1.8 * unit, 1.4 * unit)),
                               center, 2.6 * unit, 380.0 * power, (1.0, 0.96, 0.90)))
        made.append(area_light("fill", center + Vector((1.9 * unit, -1.5 * unit, 0.35 * unit)),
                               center, 3.2 * unit, 110.0 * power, (0.86, 0.92, 1.0)))
        made.append(area_light("rim", center + Vector((0.6 * unit, 2.1 * unit, 1.7 * unit)),
                               center, 1.9 * unit, 330.0 * power, (0.90, 0.95, 1.0)))
    log("lights: %d (%s)" % (len(made), preset))


# ---- camera ----------------------------------------------------------------------
def fov_tangents():
    if WIDTH >= HEIGHT:
        tan_x = SENSOR / (2.0 * LENS)
        return tan_x, tan_x * (HEIGHT / WIDTH)
    tan_y = SENSOR / (2.0 * LENS)
    return tan_y * (WIDTH / HEIGHT), tan_y


def ease(t):
    return t * t * (3.0 - 2.0 * t)


def build_camera(scene, bounds):
    lo, hi = bounds
    center = Vector(((lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5, (lo.z + hi.z) * 0.5))
    size = hi - lo
    tan_x, tan_y = fov_tangents()
    distance = max(size.x / (2.0 * tan_x), size.z / (2.0 * tan_y)) / FILL
    data = bpy.data.cameras.new("CINVAR_CAM")
    data.lens = LENS
    data.sensor_width = SENSOR
    data.sensor_fit = "AUTO"
    if float(CFG["f_stop"]) > 0.0:
        data.dof.use_dof = True
        data.dof.aperture_fstop = float(CFG["f_stop"])
        data.dof.aperture_blades = 7
    cam = bpy.data.objects.new("CINVAR_CAM", data)
    scene.collection.objects.link(cam)
    scene.camera = cam
    frames = list(range(FRAME_START, FRAME_END + 1))
    span = max(1, len(frames) - 1)
    motion = CFG["camera"]
    strength = float(CFG["camera_strength"])
    for frame in frames:
        t = (frame - FRAME_START) / float(span)
        e = ease(t)
        angle = 0.0
        dist = distance
        dx = 0.0
        dz = 0.0
        if motion == "orbit":
            angle = math.radians(13.0 * strength) * (e * 2.0 - 1.0)
        elif motion == "dolly_in":
            dist = distance * (1.15 - 0.21 * e)
        elif motion == "pan_right":
            dx = (e * 2.0 - 1.0) * 0.95 * strength
        elif motion == "handheld":
            dx = 0.055 * strength * math.sin(2.0 * math.pi * 1.7 * t)
            dz = 0.045 * strength * math.sin(2.0 * math.pi * 2.3 * t + 0.7)
            angle = math.radians(1.8) * strength * math.sin(2.0 * math.pi * 1.1 * t + 1.3)
        offset = Vector((math.sin(angle) * dist + dx,
                         -math.cos(angle) * dist,
                         0.10 * distance + dz))
        cam.location = center + offset
        direction = center - cam.location
        cam.rotation_euler = direction.to_track_quat("-Z", "Y").to_euler()
        cam.keyframe_insert("location", frame=frame)
        cam.keyframe_insert("rotation_euler", frame=frame)
        if float(CFG["f_stop"]) > 0.0:
            data.dof.focus_distance = (cam.location - center).length
            data.keyframe_insert("dof.focus_distance", frame=frame)
    if cam.animation_data is not None and cam.animation_data.action is not None:
        for fcurve in cam.animation_data.action.fcurves:
            for point in fcurve.keyframe_points:
                point.interpolation = "LINEAR"
    log("camera: %s strength=%.2f distance=%.2f centre=(%.2f, %.2f, %.2f)"
        % (motion, strength, distance, center.x, center.y, center.z))
    return cam, center


# ---- measurement ------------------------------------------------------------------
def measure(scene, cam, quad):
    from bpy_extras.object_utils import world_to_camera_view
    rows = []
    for frame in range(FRAME_START, FRAME_END + 1):
        scene.frame_set(frame)
        bpy.context.view_layer.update()
        projected = [world_to_camera_view(scene, cam, quad.matrix_world @ Vector(corner))
                     for corner in quad.bound_box]
        us = [p.x for p in projected]
        vs = [p.y for p in projected]
        inside = all(0.0 <= p.x <= 1.0 and 0.0 <= p.y <= 1.0 and p.z > 0.0 for p in projected)
        x0 = min(us) * WIDTH
        x1 = max(us) * WIDTH
        y0 = (1.0 - max(vs)) * HEIGHT
        y1 = (1.0 - min(vs)) * HEIGHT
        rows.append({"frame": frame,
                     "x": round(x0, 3), "y": round(y0, 3),
                     "width": round(x1 - x0, 3), "height": round(y1 - y0, 3),
                     "inside": bool(inside)})
    visible = all(row["inside"] for row in rows)
    parts = [ob for ob in bpy.data.objects if ob.get("cinvar_part")]
    lo, hi = world_bounds(parts)
    mid = rows[len(rows) // 2]
    mat = quad.material_slots[0].material
    image = mat.node_tree.nodes["CINVAR_VIDEO"].image
    payload = {
        "blender": bpy.app.version_string,
        "engine": scene.render.engine,
        "samples": int(CFG["samples"]),
        "screen_object": quad.name,
        "texture": os.path.join(TEX_DIR, "frame_%04d.png"),
        "texture_frames": TEX_FRAMES,
        "texture_frame_duration": int(image.frame_duration),
        "frames": rows,
        "visible_all": bool(visible),
        "device_size": [round(hi.x - lo.x, 4), round(hi.y - lo.y, 4), round(hi.z - lo.z, 4)],
        "mid_frame_rect": mid,
    }
    with open(MEASURE_PATH, "w") as fh:
        json.dump(payload, fh)
    log("measure: %d frames, screen %.0fx%.0f px at frame %d, visible_all=%s"
        % (len(rows), mid["width"], mid["height"], mid["frame"], visible))


# ---- render settings -----------------------------------------------------------------
def configure_render(scene):
    scene.render.resolution_x = WIDTH
    scene.render.resolution_y = HEIGHT
    scene.render.resolution_percentage = 100
    scene.render.fps = FPS
    scene.render.fps_base = 1.0
    scene.frame_start = FRAME_START
    scene.frame_end = FRAME_END
    scene.render.film_transparent = TRANSPARENT
    scene.render.use_motion_blur = float(CFG["motion_blur"]) > 0.0
    scene.render.motion_blur_shutter = max(0.0, float(CFG["motion_blur"]))
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA" if TRANSPARENT else "RGB"
    scene.render.image_settings.color_depth = "8"
    scene.render.use_file_extension = True
    scene.render.use_overwrite = False
    scene.render.use_placeholder = False
    scene.render.filter_size = 1.25
    set_attr(scene.render, "use_persistent_data", True)
    set_attr(scene.view_settings, "view_transform", "Standard")
    set_attr(scene.view_settings, "look", "None")
    scene.view_settings.exposure = 0.0
    scene.view_settings.gamma = 1.0
    samples = int(CFG["samples"])
    if CFG["engine"] == "eevee":
        scene.render.engine = "BLENDER_EEVEE"
        set_attr(scene.eevee, "taa_render_samples", samples)
        set_attr(scene.eevee, "use_ssr", True)
        set_attr(scene.eevee, "use_ssr_refraction", True)
        set_attr(scene.eevee, "use_gtao", True)
        set_attr(scene.eevee, "use_shadow_high_bitdepth", True)
        set_attr(scene.eevee, "use_motion_blur", True)
        set_attr(scene.eevee, "motion_blur_shutter", max(0.0, float(CFG["motion_blur"])))
    else:
        scene.render.engine = "CYCLES"
        cycles = scene.cycles
        cycles.device = "CPU"
        cycles.samples = samples
        cycles.use_adaptive_sampling = True
        cycles.adaptive_threshold = 0.02
        # Plenty of Blender builds ship without OpenImageDenoise: asking for it then makes
        # every frame fail, so the capability is probed instead of assumed.
        build = getattr(bpy.app, "build_options", None)
        denoise_ok = bool(getattr(build, "openimagedenoise", False)) if build else False
        cycles.use_denoising = bool(CFG["denoise"]) and denoise_ok
        set_attr(cycles, "denoiser", "OPENIMAGEDENOISE")
        set_attr(cycles, "denoising_use_gpu", False)
        if CFG["denoise"] and not denoise_ok:
            log("this Blender has no OpenImageDenoise; rendering without denoising")
        cycles.seed = int(CFG["seed"])
        cycles.use_animated_seed = False
        cycles.max_bounces = 6
        cycles.glossy_bounces = 4
        cycles.transmission_bounces = 4
        cycles.transparent_max_bounces = 6
        # Firefly control: these keep a low-sample render usable on a Blender built without a
        # denoiser instead of letting one glass highlight blow out the frame.
        set_attr(cycles, "blur_glossy", 1.0)
        set_attr(cycles, "sample_clamp_indirect", 2.0)
        set_attr(cycles, "sample_clamp_direct", 2.5)
        cycles.caustics_reflective = False
        cycles.caustics_refractive = False
        set_attr(cycles, "use_light_tree", True)
    log("render: %s %dx%d @%dfps frames %d..%d samples=%d denoise=%s motion_blur=%.2f transparent=%s"
        % (scene.render.engine, WIDTH, HEIGHT, FPS, FRAME_START, FRAME_END, samples,
           CFG["denoise"], float(CFG["motion_blur"]), TRANSPARENT))


def render_still(scene):
    try:
        bpy.ops.render.render(write_still=True)
    except RuntimeError as exc:
        cycles = getattr(scene, "cycles", None)
        if cycles is not None and getattr(cycles, "use_denoising", False):
            log("denoiser failed (%s); retrying without denoising" % exc)
            cycles.use_denoising = False
            bpy.ops.render.render(write_still=True)
        else:
            raise


def render(scene):
    os.makedirs(FRAMES_DIR, exist_ok=True)
    total = FRAME_END - FRAME_START + 1
    written = 0
    for frame in range(FRAME_START, FRAME_END + 1):
        path = os.path.join(FRAMES_DIR, "frame_%04d.png" % frame)
        if png_complete(path):
            continue
        scene.frame_set(frame)
        scene.render.filepath = path
        render_still(scene)
        written += 1
        if written % 5 == 0 or frame == FRAME_END:
            log("frame %d/%d rendered (%d this run)" % (frame - FRAME_START + 1, total, written))
    log("render complete: %d/%d frames on disk" % (total, total))


def main():
    scene = bpy.context.scene
    reset_scene()
    configure_render(scene)
    mats = build_materials()
    rig = bpy.data.objects.new("CINVAR_RIG", None)
    scene.collection.objects.link(rig)
    rig.location = (0.0, 0.0, RIG_Z)
    rig.rotation_euler = (math.radians(float(CFG["tilt_x_deg"])), 0.0,
                          math.radians(float(CFG["tilt_z_deg"])))
    builders = {"floating_screen": device_floating, "glass_browser": device_glass,
                "laptop": device_laptop, "phone": device_phone}
    if CFG["kind"] not in builders:
        raise SystemExit("mockup: unknown scene kind: %s" % CFG["kind"])
    quad = builders[CFG["kind"]](rig, mats)
    bpy.context.view_layer.update()
    parts = [ob for ob in bpy.data.objects if ob.get("cinvar_part")]
    bounds = world_bounds(parts)
    bounds, radius = build_environment(scene, bounds, mats)
    build_lights(bounds, radius)
    cam, _ = build_camera(scene, bounds)
    size = bounds[1] - bounds[0]
    log("scene: %s parts=%d size=(%.2f, %.2f, %.2f)"
        % (CFG["kind"], len(parts), size.x, size.y, size.z))
    measure(scene, cam, quad)
    render(scene)


main()
"##;

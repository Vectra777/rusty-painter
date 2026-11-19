"""Build the realistic figures of assets/figure-pack/v1 from MakeHuman's
CC0 assets, with Blender and the MPFB 2 add-on (see
scripts/make_figure_pack.sh, which sets both up and runs this).

    blender -b --python scripts/make_figure_pack.py -- OUT_DIR

Each figure is MPFB's base mesh shaped by its macro sliders (gender, age,
muscle, weight...), rigged with MPFB's "game_engine" skeleton (Unreal
names, three joints in every finger), re-based from MakeHuman's rest pose
(arms down and forward) to the T-pose the app's poses assume (facing +Z
once in glTF, arms straight out along X, palms down, fingers straight),
with body-shape morph targets made from the same macro sliders moved, and
written as one .glb: positions, normals, four joint weights a vertex,
morph targets (positions only), one plain material (no textures). The
hands are the male figure's, cut off at the forearm with their skeleton.

Everything used is MakeHuman/MPFB *assets* (base mesh, targets, rig and
weights), released under CC0 1.0; MPFB's code (GPL) only runs here.
"""

import importlib
import json
import math
import os
import sys
import zlib

import bmesh
import bpy
from mathutils import Matrix, Vector


def mpfb(module, name):
    """A class of the MPFB extension (its modules have a prefix)."""
    for loaded in list(sys.modules):
        if loaded.endswith(module):
            return getattr(importlib.import_module(loaded), name)
    raise SystemExit(f"MPFB isn't installed ({module} not found)")


HumanService = mpfb("mpfb.services.humanservice", "HumanService")
TargetService = mpfb("mpfb.services.targetservice", "TargetService")
HumanObjectProperties = mpfb("mpfb.entities.objectproperties", "HumanObjectProperties")

# MakeHuman's macro sliders, 0..1. Age: 0 is 1 year, 0.1875 is 11, 0.5 is
# 25, 1 is 90 (linear between).
NEUTRAL = {
    "gender": 0.5,
    "age": 0.5,
    "muscle": 0.5,
    "weight": 0.5,
    "proportions": 0.5,
    "height": 0.5,
    "cupsize": 0.5,
    "firmness": 0.5,
}

# id, name, macros, morphs (name, the macros moved). Morphs only change
# the build, not the height, so the skeleton still fits them; lengths are
# the app's own proportion sliders.
FIGURES = [
    (
        "male",
        "Male",
        {"gender": 1.0},
        [
            ("Heavy", {"weight": 1.0}),
            ("Slim", {"weight": 0.0}),
            ("Muscular", {"muscle": 1.0}),
            ("Soft", {"muscle": 0.0}),
            ("Older", {"age": 0.8}),
        ],
    ),
    (
        "female",
        "Female",
        {"gender": 0.0},
        [
            ("Heavy", {"weight": 1.0}),
            ("Slim", {"weight": 0.0}),
            ("Muscular", {"muscle": 1.0}),
            ("Soft", {"muscle": 0.0}),
            ("Older", {"age": 0.8}),
            ("Bust", {"cupsize": 1.0}),
        ],
    ),
    (
        "child",
        "Child",
        # About 8 years old.
        {"gender": 0.5, "age": 0.13},
        [
            ("Heavy", {"weight": 1.0}),
            ("Slim", {"weight": 0.0}),
        ],
    ),
]

# The skin and the eyes: plain colours (linear), for shading.
SKIN = (0.62, 0.50, 0.42, 1.0)
EYES = (0.72, 0.68, 0.64, 1.0)

FINGERS = ["index", "middle", "ring", "pinky"]


def log(*args):
    print("[figure-pack]", *args, flush=True)


def set_macros(human, macros):
    for key, value in macros.items():
        HumanObjectProperties.set_value(key, value, entity_reference=human)
    TargetService.reapply_macro_details(human)


def mixed_coords(human):
    """The vertices as the shape keys put them now."""
    key = human.shape_key_add(name="__mix", from_mix=True)
    coords = [Vector(p.co) for p in key.data]
    human.shape_key_remove(key)
    return coords


def delete_helpers(human):
    """Delete MakeHuman's helper geometry (joint cubes, clothes helpers,
    teeth...) except the eyeballs; returns the old index of each vertex
    kept, in order."""
    keep_groups = {"helper-l-eye", "helper-r-eye"}
    drop = set()
    for g in human.vertex_groups:
        if (g.name.startswith("helper-") or g.name.startswith("joint-")) and g.name not in keep_groups:
            drop.add(g.index)
    doomed = [
        v.index
        for v in human.data.vertices
        if any(e.group in drop and e.weight > 0.0 for e in v.groups)
    ]
    for m in list(human.modifiers):
        if m.type == "MASK":
            human.modifiers.remove(m)
    bm = bmesh.new()
    bm.from_mesh(human.data)
    bm.verts.ensure_lookup_table()
    doomed_set = set(doomed)
    bmesh.ops.delete(bm, geom=[bm.verts[i] for i in doomed], context="VERTS")
    bm.to_mesh(human.data)
    bm.free()
    return [i for i in range(len(doomed_set) + len(human.data.vertices)) if i not in doomed_set]


def update():
    bpy.context.view_layer.update()


def turn_bone(rig, name, rotation):
    """Turn bone `name` about its head by `rotation` (armature space)."""
    pb = rig.pose.bones[name]
    m = pb.matrix.copy()
    head = m.translation.copy()
    pb.matrix = Matrix.Translation(head) @ rotation.to_4x4() @ Matrix.Translation(-head) @ m
    update()


def bone_dir(rig, name):
    pb = rig.pose.bones[name]
    return (pb.tail - pb.head).normalized()


def aim(rig, name, direction):
    """Swing bone `name` (the least turn) to point along `direction`."""
    turn_bone(rig, name, bone_dir(rig, name).rotation_difference(direction).to_matrix())


def hand_frame(rig, side):
    """The hand's axes: along it (wrist to middle knuckle), across it
    (index knuckle to little finger knuckle) and the normal of those."""
    pb = rig.pose.bones
    wrist = pb[f"hand_{side}"].head
    along = (pb[f"middle_01_{side}"].head - wrist).normalized()
    across = pb[f"pinky_01_{side}"].head - pb[f"index_01_{side}"].head
    across = (across - along * across.dot(along)).normalized()
    return Matrix((along, across, along.cross(across))).transposed()


def t_pose(rig):
    """Pose MakeHuman's rest (arms down, elbows bent, hands turned in) as
    a T-pose: in Blender the figure faces -Y with its left at +X, so its
    arms go out along +-X, palms down (-Z), the thumb side forward (-Y),
    and the fingers straight."""
    for side, sign in (("l", 1.0), ("r", -1.0)):
        out = Vector((sign, 0.0, 0.0))
        aim(rig, f"upperarm_{side}", out)
        aim(rig, f"lowerarm_{side}", out)
        # The hand: along the arm, palm down, index side forward. The
        # forearm takes the twist (its weights spread it along the
        # forearm; a twist at the wrist would pinch it).
        # (Index to little finger is towards the back, +Y, on both hands.)
        across = Vector((0.0, 1.0, 0.0))
        target = Matrix((out, across, out.cross(across))).transposed()
        q = (target @ hand_frame(rig, side).transposed()).to_quaternion()
        along_arm = 2.0 * math.atan2(Vector((q.x, q.y, q.z)).dot(out), q.w)
        turn_bone(rig, f"lowerarm_{side}", Matrix.Rotation(along_arm, 3, out))
        twist = target @ hand_frame(rig, side).transposed()
        turn_bone(rig, f"hand_{side}", twist)
        # Each finger flat in the hand's plane, its joints in line.
        down = Vector((0.0, 0.0, -1.0))
        for finger in FINGERS:
            first = f"{finger}_01_{side}"
            d = bone_dir(rig, first)
            flat = (d - down * d.dot(down)).normalized()
            aim(rig, first, flat)
            for k in (2, 3):
                aim(rig, f"{finger}_0{k}_{side}", flat)
        # The thumb as the app's mannequin holds it (presets assume it):
        # its base a little forward of the hand, the rest of it forward
        # and out at about 40 degrees, just below the palm.
        aim(rig, f"thumb_01_{side}", Vector((sign, -0.6, -0.15)).normalized())
        thumb = Vector((sign * 0.6, -0.8, -0.1)).normalized()
        aim(rig, f"thumb_02_{side}", thumb)
        aim(rig, f"thumb_03_{side}", thumb)


def report_pose(rig):
    for name in ("upperarm_l", "lowerarm_l", "hand_l", "middle_02_l", "upperarm_r", "hand_r"):
        d = bone_dir(rig, name)
        log(f"  {name}: {d.x:+.3f} {d.y:+.3f} {d.z:+.3f}")
    f = hand_frame(rig, "l")
    log("  left hand normal (up is palm down):", tuple(round(x, 3) for x in f.col[2]))


def bake_pose_into_rest(human, rig):
    """Make the armature's pose its rest pose, with the mesh and every
    shape key moved to match (Blender can't apply an armature to a mesh
    with shape keys)."""
    keys = human.data.shape_keys.key_blocks
    posed = []
    deps = bpy.context.evaluated_depsgraph_get()
    human.show_only_shape_key = True
    for i in range(len(keys)):
        human.active_shape_key_index = i
        update()
        deps = bpy.context.evaluated_depsgraph_get()
        evaluated = human.evaluated_get(deps)
        mesh = evaluated.to_mesh()
        posed.append([Vector(v.co) for v in mesh.vertices])
        evaluated.to_mesh_clear()
    human.show_only_shape_key = False
    names = [k.name for k in keys]
    human.shape_key_clear()
    for v, co in zip(human.data.vertices, posed[0]):
        v.co = co
    human.shape_key_add(name=names[0])
    for name, coords in zip(names[1:], posed[1:]):
        key = human.shape_key_add(name=name, from_mix=False)
        for d, co in zip(key.data, coords):
            d.co = co
        key.value = 0.0
    bpy.context.view_layer.objects.active = rig
    bpy.ops.object.mode_set(mode="POSE")
    bpy.ops.pose.select_all(action="SELECT")
    bpy.ops.pose.armature_apply(selected=False)
    bpy.ops.object.mode_set(mode="OBJECT")


def ensure_eye_weights(human):
    """The eyeballs follow the head."""
    head = human.vertex_groups.get("head")
    eyes = [human.vertex_groups.get(n) for n in ("helper-l-eye", "helper-r-eye")]
    eye_ids = {g.index for g in eyes if g}
    bone_names = set(b.name for b in human.parent.data.bones)
    for v in human.data.vertices:
        if not any(e.group in eye_ids and e.weight > 0 for e in v.groups):
            continue
        weighted = any(
            human.vertex_groups[e.group].name in bone_names and e.weight > 0 for e in v.groups
        )
        if not weighted and head:
            head.add([v.index], 1.0, "REPLACE")


def materials(human):
    """One plain material for the skin, another for the eyes."""
    def make(name, color):
        mat = bpy.data.materials.new(name)
        mat.use_nodes = True
        bsdf = mat.node_tree.nodes.get("Principled BSDF")
        bsdf.inputs["Base Color"].default_value = color
        bsdf.inputs["Roughness"].default_value = 0.7
        return mat

    me = human.data
    me.materials.clear()
    me.materials.append(make("Skin", SKIN))
    me.materials.append(make("Eyes", EYES))
    eyes = {g.index for g in human.vertex_groups if g.name in ("helper-l-eye", "helper-r-eye")}
    eye_verts = {v.index for v in me.vertices if any(e.group in eyes and e.weight > 0 for e in v.groups)}
    for poly in me.polygons:
        poly.material_index = 1 if all(i in eye_verts for i in poly.vertices) else 0
    while me.uv_layers:
        me.uv_layers.remove(me.uv_layers[0])


def drop_non_bone_groups(human):
    bones = set(b.name for b in human.parent.data.bones)
    for g in list(human.vertex_groups):
        if g.name not in bones:
            human.vertex_groups.remove(g)


def clear_scene():
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete()
    for block in (bpy.data.meshes, bpy.data.armatures, bpy.data.materials):
        for item in list(block):
            block.remove(item)


def build(macros, morphs):
    clear_scene()
    human = HumanService.create_human()
    base = dict(NEUTRAL, **macros)
    variants = []
    for name, moved in morphs:
        set_macros(human, dict(base, **moved))
        variants.append((name, mixed_coords(human)))
    set_macros(human, base)
    coords = mixed_coords(human)
    human.shape_key_clear()
    for v, co in zip(human.data.vertices, coords):
        v.co = co
    rig = HumanService.add_builtin_rig(human, "game_engine")
    kept = delete_helpers(human)
    human.shape_key_add(name="Basis")
    for name, moved in variants:
        key = human.shape_key_add(name=name, from_mix=False)
        for d, old in zip(key.data, kept):
            d.co = moved[old]
        key.value = 0.0
    ensure_eye_weights(human)
    t_pose(rig)
    report_pose(rig)
    bake_pose_into_rest(human, rig)
    materials(human)
    drop_non_bone_groups(human)
    zs = [v.co.z for v in human.data.vertices]
    log(f"  height {max(zs) - min(zs):.3f} m, {len(human.data.vertices)} vertices")
    for key in human.data.shape_keys.key_blocks[1:]:
        moved = max((a.co - b.co).length for a, b in zip(key.data, human.data.vertices))
        kz = [p.co.z for p in key.data]
        log(f"  morph {key.name}: moves up to {moved * 100:.1f} cm, height {max(kz) - min(kz):.3f}")
    return human, rig


def cut_hand(human, rig, side):
    """Keep only `side`'s hand, cut straight across the forearm a little
    above the wrist, and its joints; the wrist moved to (0, 0, 1) in
    Blender (1 m up in glTF)."""
    hand_bones = {f"hand_{side}"} | {
        f"{f}_0{k}_{side}" for f in FINGERS + ["thumb"] for k in (1, 2, 3)
    }
    wrist = rig.pose.bones[f"hand_{side}"].head.copy()
    sign = 1.0 if side == "l" else -1.0
    doomed = [v.index for v in human.data.vertices if sign * (v.co.x - wrist.x) < -0.05]
    if human.data.shape_keys:
        human.shape_key_clear()
    bm = bmesh.new()
    bm.from_mesh(human.data)
    bm.verts.ensure_lookup_table()
    bmesh.ops.delete(bm, geom=[bm.verts[i] for i in doomed], context="VERTS")
    # Close the cut.
    bmesh.ops.holes_fill(bm, edges=[e for e in bm.edges if e.is_boundary], sides=0)
    bm.to_mesh(human.data)
    bm.free()
    for g in list(human.vertex_groups):
        if g.name not in hand_bones:
            human.vertex_groups.remove(g)
    # The bit of forearm left goes with the hand.
    hand = human.vertex_groups[f"hand_{side}"]
    for v in human.data.vertices:
        if sum(e.weight for e in v.groups) < 1e-3:
            hand.add([v.index], 1.0, "REPLACE")
    bpy.context.view_layer.objects.active = rig
    bpy.ops.object.mode_set(mode="EDIT")
    edit = rig.data.edit_bones
    for b in list(edit):
        if b.name not in hand_bones:
            edit.remove(b)
    bpy.ops.object.mode_set(mode="OBJECT")
    offset = Vector((0.0, 0.0, 1.0)) - wrist
    rig.location = rig.location + offset
    update()


def export(path, objects):
    bpy.ops.object.select_all(action="DESELECT")
    for o in objects:
        o.select_set(True)
    bpy.context.view_layer.objects.active = objects[0]
    bpy.ops.export_scene.gltf(
        filepath=path,
        export_format="GLB",
        use_selection=True,
        export_texcoords=False,
        export_normals=True,
        export_tangents=False,
        export_materials="EXPORT",
        export_image_format="NONE",
        export_skins=True,
        export_influence_nb=4,
        export_all_influences=False,
        export_morph=True,
        export_morph_normal=False,
        export_morph_tangent=False,
        export_animations=False,
        export_yup=True,
        export_apply=False,
        export_extras=False,
        export_cameras=False,
        export_lights=False,
    )
    log("  wrote", path, os.path.getsize(path), "bytes")


def main():
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    if len(argv) != 1:
        raise SystemExit("usage: blender -b --python make_figure_pack.py -- OUT_DIR")
    out = os.path.abspath(argv[0])
    os.makedirs(out, exist_ok=True)
    for fid, name, macros, morphs in FIGURES:
        log(name)
        human, rig = build(macros, morphs)
        human.name = name
        rig.name = f"{name} skeleton"
        export(os.path.join(out, f"{fid}.glb"), [rig, human])
    for side, word in (("l", "left"), ("r", "right")):
        log(f"Hand {word}")
        human, rig = build(FIGURES[0][2], [])
        cut_hand(human, rig, side)
        human.name = f"{word.capitalize()} hand"
        rig.name = f"{word.capitalize()} hand skeleton"
        export(os.path.join(out, f"hand-{word}.glb"), [rig, human])
    write_manifest(out)


def write_manifest(out):
    """The pack's manifest (src/canvas/figure/pack.rs reads it), with the
    CRC-32 of the model files one after the other as its `content`."""
    commit = os.environ.get("MPFB_COMMIT", "unknown")
    source = (
        "MakeHuman / MPFB 2 assets (base mesh, targets, game_engine rig and weights), "
        f"https://github.com/makehumancommunity/mpfb2 at {commit[:7]}; "
        "built by scripts/make_figure_pack.py"
    )
    models = [(fid, name, "body") for fid, name, _, _ in FIGURES] + [
        ("hand-left", "Left hand", "hand"),
        ("hand-right", "Right hand", "hand"),
    ]
    crc = 0
    entries = []
    for fid, name, kind in models:
        with open(os.path.join(out, f"{fid}.glb"), "rb") as f:
            crc = zlib.crc32(f.read(), crc)
        entries.append(
            {
                "id": fid,
                "name": name,
                "file": f"{fid}.glb",
                "kind": kind,
                "licence": "CC0-1.0",
                "source": source,
            }
        )
    manifest = {"version": 1, "id": "figures-v1", "models": entries, "content": f"{crc:08x}"}
    with open(os.path.join(out, "manifest.json"), "w") as f:
        f.write(json.dumps(manifest, indent=2) + "\n")
    log("manifest content", manifest["content"])


main()

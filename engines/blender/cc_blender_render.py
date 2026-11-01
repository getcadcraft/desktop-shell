# SPDX-License-Identifier: Apache-2.0
"""
cc_blender_render.py -- CadCraft Studio's Blender engine (0.8 RE.4, docs/0.8-PART1.md D1-D3).

Shipped inside Studio (<resources>/engines/blender/) and run only by Studio's engines.rs:

    blender --background --factory-startup --noaudio --python-exit-code 3 --python cc_blender_render.py -- <job file>

The job file (cc-engine-job.json) is written by Rust from typed fields; the package beside it holds scene.usdc (or .usdz) and
cadcraft-render.json (the manifest, docs/USD-CONVENTIONS.md 10.1).  The script imports the USD for geometry, instancing and
materials, then applies the manifest over it, so nothing depends on how much of the USD an importer keeps: it builds the sun, the sky,
the lamps (with their IES profiles), the camera, the ground, the exact OpenPBR values per material, the exposure and the size
itself.  Output: out/result.png (or .exr), or out/frames.zip (a stored zip of the frames) for an animation.

It talks to Studio only by printing lines:  CCPROGRESS 0.42 | CCRESULT out/result.png | CCERROR <code>.
Exit code 3 = the script raised (Blender's --python-exit-code).  Nothing is read from outside the job folder.

Calibration: SUN_STRENGTH_PER_UNIT, LAMP_WATTS_PER_CANDELA and AREA_WATTS_PER_FLUX are the first mapping from our scene units to
Blender's; RE.8 (checks across engines) measures the pictures against our path tracer and these three are where it is corrected.
"""
import json
import math
import os
import sys
import zipfile

SUN_STRENGTH_PER_UNIT = 1.0      # our sun intensity (clear sky 3.2) -> Blender sun strength (W/m2)
LAMP_WATTS_PER_CANDELA = 4 * math.pi * math.pi   # Blender's point light: I = P / (4 pi^2); our lamps are in candela of the render's scale
AREA_WATTS_PER_FLUX = 1.0        # an area light emits our flux (lumens * scale) as its power
ENGINE_NAME = 'CadCraft'


def say(line):
    sys.stdout.write(line + '\n')
    sys.stdout.flush()


def fail(code):
    say('CCERROR ' + code)
    raise SystemExit(3)


def rel_ok(name):
    return (isinstance(name, str) and name and not name.startswith('/') and '\\' not in name and ':' not in name
            and all(p not in ('', '.', '..') for p in name.split('/')))


def load_job(path):
    with open(path, 'r', encoding='utf-8') as f:
        job = json.load(f)
    if job.get('format') != 'cadcraft-engine-job-1':
        fail('bad_job')
    if job.get('engine') not in ('cycles', 'eevee') or job.get('output') not in ('png', 'exr') or job.get('device') not in ('cpu', 'gpu'):
        fail('bad_job')
    if not rel_ok(job.get('manifest')) or job.get('out') != 'out':
        fail('bad_job')
    return job


def load_manifest(folder, name):
    with open(os.path.join(folder, name), 'r', encoding='utf-8') as f:
        m = json.load(f)
    if m.get('format') != 'cadcraft-render-1':
        fail('bad_manifest')
    scene = m.get('scene') or {}
    if not rel_ok(scene.get('file')) or not os.path.isfile(os.path.join(folder, scene['file'])):
        fail('no_scene')
    for key in ('camera', 'sun', 'sky', 'exposure'):
        if not isinstance(m.get(key), dict):
            fail('bad_manifest')
    cam = m['camera']
    if len(cam.get('world') or []) != 16:
        fail('bad_manifest')
    if cam.get('projection') not in ('perspective', 'orthographic'):
        fail('projection_unsupported')
    return m


# ---- pure maths (testable without Blender) ----------------------------------------------------------------------

def sensor_lens(fov_y_degrees, sensor_height=24.0):
    """Focal length (mm) for a vertical field of view on a sensor of this height."""
    return sensor_height / 2.0 / math.tan(math.radians(fov_y_degrees) / 2.0)


def hex_to_rgb(h, default=(0.8, 0.8, 0.8)):
    """'#rrggbb' (sRGB) -> linear Rec.709 triple."""
    try:
        n = int(str(h)[1:7], 16)
    except ValueError:
        return default

    def c(v):
        v /= 255.0
        return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4
    return (c((n >> 16) & 255), c((n >> 8) & 255), c(n & 255))


def lamp_watts(lamp):
    """Blender watts for one manifest lamp (see the calibration note above)."""
    lumens = max(0.0, float(lamp.get('lumens', 0)))
    if lamp.get('kind') == 'area':
        return lumens * 0.02 * AREA_WATTS_PER_FLUX
    return lumens / (4 * math.pi) * 0.02 * LAMP_WATTS_PER_CANDELA


# ---- Blender side -----------------------------------------------------------------------------------------------

def blender_modules():
    import bpy
    import mathutils
    return bpy, mathutils


def accepted(op, **kw):
    """Only the keyword arguments this Blender's operator has (the USD importer's options differ between versions)."""
    props = set(op.get_rna_type().properties.keys())
    return {k: v for k, v in kw.items() if k in props}


def clear_scene(bpy):
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete()
    for coll in (bpy.data.meshes, bpy.data.lights, bpy.data.cameras, bpy.data.materials):
        for item in list(coll):
            coll.remove(item)


def import_usd(bpy, path):
    kw = accepted(bpy.ops.wm.usd_import, filepath=path, import_cameras=False, import_lights=False, import_materials=True,
                  import_usd_preview=True, set_material_blend=True, import_all_materials=True, import_meshes=True,
                  import_volumes=False, import_shapes=True, relative_path=False)
    res = bpy.ops.wm.usd_import(**kw)
    if 'FINISHED' not in res:
        fail('usd_import_failed')


def principled(mat):
    mat.use_nodes = True
    for n in mat.node_tree.nodes:
        if n.type == 'BSDF_PRINCIPLED':
            return n
    n = mat.node_tree.nodes.new('ShaderNodeBsdfPrincipled')
    out = next((x for x in mat.node_tree.nodes if x.type == 'OUTPUT_MATERIAL'), None) or mat.node_tree.nodes.new('ShaderNodeOutputMaterial')
    mat.node_tree.links.new(n.outputs['BSDF'], out.inputs['Surface'])
    return n


def set_input(node, names, value):
    """Set the first input of these names that this Blender's Principled BSDF has (4.x renamed several)."""
    for n in names:
        sock = node.inputs.get(n)
        if sock is not None:
            sock.default_value = value
            return True
    return False


OPENPBR_TO_PRINCIPLED = {
    'base_weight': None,
    'base_metalness': (['Metallic'], 'f'),
    'specular_roughness': (['Roughness'], 'f'),
    'specular_ior': (['IOR'], 'f'),
    'specular_weight': (['Specular IOR Level', 'Specular'], 'f'),
    'transmission_weight': (['Transmission Weight', 'Transmission'], 'f'),
    'coat_weight': (['Coat Weight', 'Clearcoat'], 'f'),
    'coat_roughness': (['Coat Roughness', 'Clearcoat Roughness'], 'f'),
    'fuzz_weight': (['Sheen Weight', 'Sheen'], 'f'),
    'geometry_opacity': (['Alpha'], 'f'),
    'subsurface_weight': (['Subsurface Weight', 'Subsurface'], 'f'),
    'thin_film_weight': None,
}


def apply_material(bpy, m):
    """One manifest material: the exact OpenPBR values onto the imported material's Principled BSDF."""
    name = m['prim'].rsplit('/', 1)[-1] if m.get('prim') else ''
    mat = bpy.data.materials.get(name)
    if not mat:
        return False
    node = principled(mat)
    op = m.get('openpbr') or {}
    for key, value in op.items():
        if key == 'base_color':
            set_input(node, ['Base Color'], tuple(value) + (1.0,))
        elif key == 'emission_color':
            set_input(node, ['Emission Color', 'Emission'], tuple(value) + (1.0,))
        elif key == 'emission_luminance':
            set_input(node, ['Emission Strength'], float(value))
        elif key == 'transmission_color':
            pass   # Principled has no tint on transmission: the base colour carries it
        elif key == 'geometry_thin_walled':
            pass
        elif key in OPENPBR_TO_PRINCIPLED and OPENPBR_TO_PRINCIPLED[key]:
            names, _ = OPENPBR_TO_PRINCIPLED[key]
            set_input(node, names, float(value))
    if float(op.get('geometry_opacity', 1.0)) < 0.999:
        mat.blend_method = 'BLEND' if hasattr(mat, 'blend_method') else None
    return True


def make_camera(bpy, mathutils, m, w, h):
    c = m['camera']
    data = bpy.data.cameras.new('CadCraft camera')
    obj = bpy.data.objects.new('CadCraft camera', data)
    bpy.context.scene.collection.objects.link(obj)
    data.sensor_fit = 'VERTICAL'
    data.sensor_height = 24.0
    if c['projection'] == 'orthographic':
        data.type = 'ORTHO'
        data.ortho_scale = float(c.get('orthoHeight', 10.0))
    else:
        data.lens = sensor_lens(float(c.get('fovY', 50.0)))
    data.clip_start = 0.05
    data.clip_end = 20000.0
    w4 = c['world']   # column-major, camera to world, looks down -Z: Blender's own convention
    obj.matrix_world = mathutils.Matrix([[w4[0], w4[4], w4[8], w4[12]], [w4[1], w4[5], w4[9], w4[13]], [w4[2], w4[6], w4[10], w4[14]], [0, 0, 0, 1]])
    bpy.context.scene.camera = obj
    return obj


def make_sun(bpy, mathutils, m):
    s = m['sun']
    if not s.get('on'):
        return None
    d = mathutils.Vector(s['dir'])
    data = bpy.data.lights.new('CadCraft sun', 'SUN')
    data.energy = float(s.get('intensity', 3.2)) * SUN_STRENGTH_PER_UNIT
    data.angle = math.radians(float(s.get('diameterDeg', 0.53)))
    data.color = hex_to_rgb(s.get('tint', '#ffffff'))
    obj = bpy.data.objects.new('CadCraft sun', data)
    bpy.context.scene.collection.objects.link(obj)
    obj.rotation_euler = (-d).to_track_quat('-Z', 'Y').to_euler()   # the lamp shines along its -Z, away from the sun's direction
    return obj


def make_lamps(bpy, mathutils, m, folder):
    made = []
    for L in m.get('lamps') or []:
        kind = {'point': 'POINT', 'spot': 'SPOT', 'area': 'AREA'}.get(L.get('kind'), 'POINT')
        data = bpy.data.lights.new(L.get('name') or 'Lamp', kind)
        data.energy = lamp_watts(L)
        data.color = tuple(max(0.0, min(1.0, c)) for c in L.get('color', (1, 1, 1)))
        if kind == 'SPOT':
            data.spot_size = math.radians(float(L.get('angle', 90)))
        if kind == 'AREA':
            data.shape = 'RECTANGLE'
            data.size, data.size_y = (L.get('size') or [0.5, 0.5])[:2]
        obj = bpy.data.objects.new(L.get('name') or 'Lamp', data)
        bpy.context.scene.collection.objects.link(obj)
        obj.location = tuple(L.get('p', (0, 0, 0)))
        if kind != 'POINT':
            obj.rotation_euler = mathutils.Vector(L.get('dir', (0, 0, -1))).to_track_quat('-Z', 'Y').to_euler()
        ies = L.get('ies')
        if ies and rel_ok(ies) and os.path.isfile(os.path.join(folder, ies)):
            data.use_nodes = True
            nt = data.node_tree
            tex = nt.nodes.new('ShaderNodeTexIES')
            tex.mode = 'EXTERNAL'
            tex.filepath = os.path.join(folder, ies)
            emit = next((n for n in nt.nodes if n.type == 'EMISSION'), None)
            if emit:
                nt.links.new(tex.outputs['Fac'], emit.inputs['Strength'])
        made.append(obj)
    return made


def make_world(bpy, m):
    sky = m['sky']
    world = bpy.data.worlds.new('CadCraft sky') if not bpy.context.scene.world else bpy.context.scene.world
    bpy.context.scene.world = world
    world.use_nodes = True
    nt = world.node_tree
    for n in list(nt.nodes):
        nt.nodes.remove(n)
    out = nt.nodes.new('ShaderNodeOutputWorld')
    bg = nt.nodes.new('ShaderNodeBackground')
    kind = sky.get('kind', 'clear')
    if kind in ('clear', 'physical', 'dusk') and not sky.get('plain'):
        tex = nt.nodes.new('ShaderNodeTexSky')
        tex.sky_type = 'NISHITA'
        tex.sun_disc = False   # the sun lamp is the sun (one sun, not two)
        d = m['sun'].get('dir') or [0, 0, 1]
        tex.sun_elevation = math.asin(max(-1.0, min(1.0, d[2])))
        tex.sun_rotation = math.atan2(d[0], d[1]) if (d[0] or d[1]) else 0.0
        if hasattr(tex, 'air_density') and sky.get('turbidity'):
            tex.dust_density = max(0.0, (float(sky['turbidity']) - 2.0) / 2.0)
        nt.links.new(tex.outputs['Color'], bg.inputs['Color'])
    elif kind == 'hdri' and sky.get('hdri'):
        bg.inputs['Color'].default_value = (*hex_to_rgb(sky.get('horizon', '#c4d6e8')), 1.0)   # the picture is not in the package yet: its horizon colour
    else:
        bg.inputs['Color'].default_value = (*hex_to_rgb(sky.get('horizon', '#dfe2e5')), 1.0)
    bg.inputs['Strength'].default_value = float(sky.get('intensity', 0.45)) * (1.0 if kind != 'night' else 1.0)
    nt.links.new(bg.outputs['Background'], out.inputs['Surface'])


def make_ground(bpy, m):
    z = (m.get('scene') or {}).get('groundZ')
    if not m.get('ground') or z is None:
        return None
    mesh = bpy.data.meshes.new('CadCraft ground')
    s = 4000.0
    mesh.from_pydata([(-s, -s, 0), (s, -s, 0), (s, s, 0), (-s, s, 0)], [], [(0, 1, 2, 3)])
    obj = bpy.data.objects.new('CadCraft ground', mesh)
    bpy.context.scene.collection.objects.link(obj)
    obj.location.z = z
    mat = bpy.data.materials.new('CadCraft ground')
    node = principled(mat)
    spec = next((x for x in m.get('materials', []) if x.get('key') == '__ground'), {'openpbr': {}})
    op = spec.get('openpbr') or {}
    set_input(node, ['Base Color'], tuple(op.get('base_color', (0.6, 0.58, 0.52))) + (1.0,))
    set_input(node, ['Roughness'], float(op.get('specular_roughness', 1.0)))
    mesh.materials.append(mat)
    return obj


def pick_device(bpy, scene, want):
    """GPU if asked and one is there (OptiX, CUDA, HIP, Metal, oneAPI), else the CPU, said so."""
    if want != 'gpu':
        scene.cycles.device = 'CPU'
        return 'cpu'
    prefs = bpy.context.preferences.addons['cycles'].preferences
    for kind in ('OPTIX', 'CUDA', 'HIP', 'METAL', 'ONEAPI'):
        try:
            prefs.compute_device_type = kind
            prefs.get_devices()
            if any(d.type == kind for d in prefs.devices):
                for d in prefs.devices:
                    d.use = d.type == kind
                scene.cycles.device = 'GPU'
                return kind.lower()
        except Exception:   # noqa: BLE001 - a Blender without that back end
            continue
    scene.cycles.device = 'CPU'
    say('CCNOTE no_gpu_using_cpu')
    return 'cpu'


def configure_render(bpy, job, m):
    scene = bpy.context.scene
    scene.render.resolution_x = int(job['width'])
    scene.render.resolution_y = int(job['height'])
    scene.render.resolution_percentage = 100
    ex = m.get('exposure') or {}
    scene.view_settings.view_transform = 'Standard'
    scene.view_settings.exposure = float(ex.get('stops', 0.0))
    scene.render.film_transparent = False
    if job['engine'] == 'cycles':
        scene.render.engine = 'CYCLES'
        scene.cycles.samples = int(job['samples'])
        scene.cycles.use_denoising = bool(job['denoise'])
        pick_device(bpy, scene, job['device'])
    else:
        names = [e.identifier for e in bpy.types.RenderSettings.bl_rna.properties['engine'].enum_items]
        scene.render.engine = 'BLENDER_EEVEE_NEXT' if 'BLENDER_EEVEE_NEXT' in names else 'BLENDER_EEVEE'
        try:
            scene.eevee.taa_render_samples = int(job['samples'])
        except AttributeError:
            pass
    fmt = scene.render.image_settings
    fmt.file_format = 'PNG' if job['output'] == 'png' else 'OPEN_EXR'
    if job['output'] == 'png':
        fmt.color_depth = '8'
        fmt.color_mode = 'RGB'
    return scene


def render_still(bpy, job, folder):
    scene = bpy.context.scene
    ext = 'png' if job['output'] == 'png' else 'exr'
    scene.render.filepath = os.path.join(folder, 'out', 'result.' + ext)
    bpy.ops.render.render(write_still=True)
    if not os.path.isfile(scene.render.filepath):
        fail('no_picture')
    say('CCPROGRESS 1')
    say('CCRESULT out/result.' + ext)


def render_frames(bpy, mathutils, job, m, folder, cam):
    """An animation in one run: the camera keyed per frame from the manifest, the frames zipped (stored) into out/frames.zip."""
    scene = bpy.context.scene
    first, last, fps = job['frames']
    scene.render.fps = int(fps)
    cams = (m.get('frames') or {}).get('cameras') or []
    n = last - first + 1
    if cams and len(cams) != n:
        fail('bad_manifest')
    paths = []
    for i in range(n):
        frame = first + i
        scene.frame_set(frame)
        if cams:
            w4 = cams[i]['world']
            cam.matrix_world = mathutils.Matrix([[w4[0], w4[4], w4[8], w4[12]], [w4[1], w4[5], w4[9], w4[13]], [w4[2], w4[6], w4[10], w4[14]], [0, 0, 0, 1]])
        p = os.path.join(folder, 'out', 'frame_%05d.png' % i)
        scene.render.filepath = p
        bpy.ops.render.render(write_still=True)
        if not os.path.isfile(p):
            fail('no_picture')
        paths.append(p)
        say('CCPROGRESS %.4f' % ((i + 1) / float(n)))
    zpath = os.path.join(folder, 'out', 'frames.zip')
    with zipfile.ZipFile(zpath, 'w', zipfile.ZIP_STORED) as z:
        for p in paths:
            z.write(p, os.path.basename(p))
            os.remove(p)
    say('CCRESULT out/frames.zip')


def build_scene(bpy, mathutils, job, m, folder):
    """The whole scene from the package: the USD, then the manifest over it. Used by the render (here) and by Open in Blender."""
    clear_scene(bpy)
    import_usd(bpy, os.path.join(folder, m['scene']['file']))
    say('CCPROGRESS 0.1')
    for mat in m.get('materials') or []:
        apply_material(bpy, mat)
    make_world(bpy, m)
    sun = make_sun(bpy, mathutils, m)
    lamps = make_lamps(bpy, mathutils, m, folder)
    say('CCINFO lamps=%d sun=%d' % (len(lamps), 1 if sun else 0))   # what was made, for the cross-engine check (RE.8)
    make_ground(bpy, m)
    cam = make_camera(bpy, mathutils, m, job['width'], job['height'])
    configure_render(bpy, job, m)
    return cam


def main(argv):
    if '--' not in argv or len(argv) <= argv.index('--') + 1:
        fail('bad_job')
    job_path = argv[argv.index('--') + 1]
    folder = os.path.dirname(os.path.abspath(job_path))
    job = load_job(job_path)
    m = load_manifest(folder, job['manifest'])
    bpy, mathutils = blender_modules()
    say('CCPROGRESS 0.02')
    cam = build_scene(bpy, mathutils, job, m, folder)
    say('CCPROGRESS 0.15')
    if job.get('frames'):
        render_frames(bpy, mathutils, job, m, folder, cam)
    else:
        render_still(bpy, job, folder)


if __name__ == '__main__':
    try:
        main(sys.argv)
    except SystemExit:
        raise
    except Exception as e:   # noqa: BLE001 - anything else is the script failing; Studio maps exit code 3 to script_failed
        say('CCERROR script_failed')
        say(type(e).__name__ + ': ' + str(e)[:300])
        raise SystemExit(3)

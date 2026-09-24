"""Rebuild with Blender 4.5: blender --background --factory-startup --python simplify.py.

Source: Fiat 500 - 1970 Model, Parsa Farvadian, CC BY 4.0.
The original is retained verbatim in source/. All distances retain source units.
"""
import json
import math
import sys
from pathlib import Path

import bpy
import bmesh
from mathutils import Vector

ROOT = Path(__file__).resolve().parent
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT))
from rear_vents import replace_and_bake
from headlights import round_headlights
from fender_wells import add_fender_wells
from mathutils.bvhtree import BVHTree
SOURCE = ROOT / 'source/fiat-500-original.glb'
OUTPUT = ROOT / 'fiat-500-simplified.glb'

# Names before the material suffix in Sketchfab's download.
REMOVE = {'Interior Black', 'Driver Seat', 'Passenger Seat', 'Steering Wheel',
          'Front Rollbar', 'Rear Rollbar', 'Front Bumper Holders', 'Rear Bumper Holders'}
TARGETS = {
    'Main Chassis': 6000, 'Hood': 1200, 'Roof': 1200, 'Doors': 2000,
    'Rear Trunk Bumper': 160,
    'Front Orange Lights': 140, 'Front Side Lights': 80,
    'Front Side Lights Chrome': 100,
    'Rear Trunk Chrome': 70, 'Tail Lights Chrome': 120, 'Tail Lights Red': 120,
    'Tail Lights Chrome Parts': 160, 'Tail Lights Hazard': 100,
    'Door Handles': 160, 'Side Mirror Stands': 100, 'Side Mirrors': 160,
    'Front Bumper': 240, 'Rear Bumper': 220,
    'Front Bumper Screws': 48, 'Rear Bumper Screws': 48, 'Front Screws': 48,
    'Front Logo Chrome.001': 80, 'Front Logo Chrome': 120,
    'Rear Trunk Handle': 80,
    'Rear Window Border': 140, 'Rear Side WIndow Borders': 240,
    'Front Side Windows Chrome': 280,
    'Front WIndow': 160, 'Front Side WIndows': 120,
    'Rear Side Windows': 100, 'Rear Window': 80,
    'Side Mirrors Glass': 48,
}


def triangles(obj):
    obj.data.calc_loop_triangles()
    return len(obj.data.loop_triangles)


def simplify_tire(obj):
    """Revolve a reduced version of the original tire section, without fine tread.

    Uniform rings preserve circular silhouettes better than collapsing the many
    narrow tread strips. The section below was measured from this source mesh.
    """
    coords = [v.co for v in obj.data.vertices]
    center = Vector([(min(v[a] for v in coords)+max(v[a] for v in coords))/2 for a in range(3)])
    half_width = (max(v.x for v in coords)-min(v.x for v in coords))/2
    radius = max((Vector((v.y-center.y, v.z-center.z))).length for v in coords)
    section = [(-.803,.600),(-.945,.730),(-1,.873),(-1,.931),
               (-.896,.989),(-.700,1),(.700,1),(.896,.989),
               (1,.931),(1,.873),(.945,.730),(.803,.600)]
    segments = 64
    vertices = [(center.x + x*half_width,
                 center.y + r*radius*math.cos(2*math.pi*k/segments),
                 center.z + r*radius*math.sin(2*math.pi*k/segments))
                for x,r in section for k in range(segments)]
    faces = []
    for band in range(len(section)):
        next_band = (band+1)%len(section)
        for k in range(segments):
            n=(k+1)%segments
            faces.append((band*segments+k, band*segments+n,
                          next_band*segments+n, next_band*segments+k))
    mesh=bpy.data.meshes.new(obj.data.name+' simplified rings')
    mesh.from_pydata(vertices, [], faces)
    for material in obj.data.materials:
        mesh.materials.append(material)
    obj.data=mesh
    bm=bmesh.new(); bm.from_mesh(mesh)
    bmesh.ops.recalc_face_normals(bm, faces=list(bm.faces))
    bm.to_mesh(mesh); bm.free()
    for face in mesh.polygons:
        face.use_smooth=True


def main():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.context.preferences.filepaths.save_version = 0
    bpy.ops.import_scene.gltf(filepath=str(SOURCE))
    objects = [o for o in bpy.context.scene.objects if o.type == 'MESH']
    before = sum(triangles(o) for o in objects)
    report = {'source_triangles': before, 'source_bytes': SOURCE.stat().st_size,
              'removed': [], 'meshes': []}

    # Bake the hierarchy without changing world-space scale, orientation, or position.
    for obj in objects:
        matrix = obj.matrix_world.copy()
        obj.parent = None
        obj.matrix_world = matrix
    for obj in list(bpy.context.scene.objects):
        if obj.type != 'MESH':
            bpy.data.objects.remove(obj, do_unlink=True)

    for obj in objects:
        name = obj.name.split('_')[0]
        original_triangles = triangles(obj)
        if name in REMOVE:
            report['removed'].append({'name': name, 'triangles': original_triangles})
            bpy.data.objects.remove(obj, do_unlink=True)
            continue
        bpy.ops.object.select_all(action='DESELECT')
        obj.select_set(True)
        bpy.context.view_layer.objects.active = obj
        bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)

        if name in {'Front Headlights','Front Headlight Chrome'}:
            round_headlights(obj, lens=name=='Front Headlights')
            report['meshes'].append({'name':obj.name,'before':original_triangles,
                                     'after':triangles(obj),'method':'evenly spaced circular rings'})
            continue
        if name in {'Back Vent','Trunk'}:
            result = replace_and_bake(obj, ROOT)
            report['meshes'].append({'name':obj.name,'before':original_triangles,**result})
            continue
        if '-Tire' in name:
            simplify_tire(obj)
            report['meshes'].append({'name': obj.name, 'before': original_triangles,
                                     'after': triangles(obj)})
            continue
        elif '-Rim' in name:
            target = 1200
        elif name == 'Rear License Plate':
            material = obj.data.materials[0].name
            target = 100 if 'Border' in material else 32
        else:
            target = TARGETS[name]
        modifier = obj.modifiers.new('Reduce triangles', 'DECIMATE')
        modifier.decimate_type = 'COLLAPSE'
        modifier.ratio = min(1.0, target / triangles(obj))
        modifier.use_collapse_triangulate = True
        if '-Tire' not in name and '-Rim' not in name:
            modifier.use_symmetry = True
            modifier.symmetry_axis = 'X'
        bpy.ops.object.modifier_apply(modifier=modifier.name)
        report['meshes'].append({'name': obj.name, 'before': original_triangles,
                                 'after': triangles(obj)})

    # The smooth cover replaces a recessed license-plate mount. Move the plate
    # assembly just outside the new surface, preserving its internal alignment.
    cover=next(o for o in bpy.context.scene.objects if o.name.startswith('Trunk_'))
    cover.data.calc_loop_triangles()
    tree=BVHTree.FromPolygons([v.co for v in cover.data.vertices],
                             [t.vertices for t in cover.data.loop_triangles],all_triangles=True)
    plates=[o for o in bpy.context.scene.objects if o.name.startswith('Rear License Plate_')]
    clearance=0
    for obj in plates:
        for v in obj.data.vertices:
            hit,_,_,_=tree.ray_cast(Vector((v.co.x,5,v.co.z)),Vector((0,-1,0)),10)
            if hit is not None:clearance=max(clearance,hit.y-v.co.y+.004)
    for obj in plates:
        for v in obj.data.vertices:v.co.y+=clearance
    report['license_plate_clearance_adjustment']=clearance

    # A tinted opaque surface hides the now-empty cabin in any renderer.
    glass = bpy.data.materials['Glass']
    principled = next(n for n in glass.node_tree.nodes if n.type == 'BSDF_PRINCIPLED')
    principled.inputs['Base Color'].default_value = (0.055, 0.075, 0.095, 1)
    principled.inputs['Alpha'].default_value = 1
    principled.inputs['Transmission Weight'].default_value = 0
    principled.inputs['Metallic'].default_value = 0.25
    principled.inputs['Roughness'].default_value = 0.22
    glass.diffuse_color = (0.055, 0.075, 0.095, 1)

    report['meshes'].extend(add_fender_wells())

    # Only retained meshes and their used materials go into the deliverables.
    bpy.data.orphans_purge(do_recursive=True)
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.export_scene.gltf(filepath=str(OUTPUT), export_format='GLB',
        use_selection=True, export_animations=False, export_cameras=False,
        export_lights=False, export_extras=False,
        export_copyright='Fiat 500 - 1970 Model by Parsa Farvadian, CC BY 4.0. Modified: simplified geometry, interior and undercarriage removed, opaque windows, textured rear vents, round headlights, painted wheel wells.')
    bpy.ops.wm.save_as_mainfile(filepath=str(ROOT / 'fiat-500-simplified.blend'))
    report['output_triangles'] = sum(m['after'] for m in report['meshes'])
    report['output_bytes'] = OUTPUT.stat().st_size
    report['triangle_reduction_percent'] = round(100 * (1 - report['output_triangles'] / before), 2)
    (ROOT / 'simplification-report.json').write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps({k: v for k, v in report.items() if k not in {'removed','meshes'}}))


if __name__ == '__main__':
    main()

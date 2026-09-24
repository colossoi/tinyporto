"""Replace perforated rear panels with smooth hulls and bake the source detail."""
from pathlib import Path

import bpy
import bmesh
import numpy as np
from mathutils import Vector
from mathutils.geometry import delaunay_2d_cdt


def replace_and_bake(obj, root):
    scene = bpy.context.scene
    reference = obj.copy()
    reference.data = obj.data.copy()
    scene.collection.objects.link(reference)
    reference.name = obj.name + ' bake source'
    reference.select_set(False)

    # Bridge the slats with a smooth exterior envelope. Weld numerical noise
    # before constructing the hull, retaining the actual panel perimeter.
    bm = bmesh.new()
    for vertex in obj.data.vertices:
        bm.verts.new(vertex.co)
    bmesh.ops.remove_doubles(bm, verts=list(bm.verts), dist=.0001)
    bmesh.ops.convex_hull(bm, input=list(bm.verts), use_existing_faces=False)
    bmesh.ops.delete(bm, geom=[v for v in bm.verts if not v.link_faces], context='VERTS')
    bm.normal_update()
    bmesh.ops.delete(bm, geom=[f for f in bm.faces if f.normal.y < .001], context='FACES')
    bmesh.ops.delete(bm, geom=[v for v in bm.verts if not v.link_faces], context='VERTS')
    bmesh.ops.dissolve_degenerate(bm, edges=list(bm.edges), dist=.00001)
    mesh = bpy.data.meshes.new(obj.data.name + ' smooth shell')
    bm.to_mesh(mesh)
    bm.free()
    obj.data = mesh
    for face in mesh.polygons:
        face.use_smooth = True

    bpy.ops.object.select_all(action='DESELECT')
    obj.select_set(True)
    bpy.context.view_layer.objects.active = obj
    mesh.calc_loop_triangles()
    budget=96 if obj.name.startswith('Back Vent') else 224
    modifier=obj.modifiers.new('Reduce smooth shell','DECIMATE')
    modifier.ratio=min(1,budget/len(mesh.loop_triangles))
    modifier.use_collapse_triangulate=True
    bpy.ops.object.modifier_apply(modifier=modifier.name)
    mesh=obj.data
    # Retriangulate the outward samples as one continuous height field. This
    # removes tiny pinched loops from nearly coplanar hull faces, without
    # changing the fitted perimeter or reopening any slots.
    heights={}
    for v in mesh.vertices:
        key=(round(v.co.x,5),round(v.co.z,5))
        heights[key]=max(heights.get(key,-float('inf')),v.co.y)
    points=list(heights)
    planar,_,faces,orig,_,_=delaunay_2d_cdt([Vector(p) for p in points],[],[],0,1e-7)
    vertices=[(p.x,max(heights[points[i]] for i in ids),p.y) for p,ids in zip(planar,orig)]
    # Fit the broad curvature, suppressing louver lips and numerical hull
    # wrinkles. Upper-envelope samples avoid fitting into the old recesses.
    source=np.array([v.co[:] for v in reference.data.vertices])
    bounds_lo,bounds_hi=source.min(axis=0),source.max(axis=0)
    mid=(bounds_hi+bounds_lo)/2;half=(bounds_hi-bounds_lo)/2
    bins={}
    for p in source:
        key=(int(round((p[0]-mid[0])/half[0]*32)),int(round((p[2]-mid[2])/half[2]*32)))
        if key not in bins or p[1]>bins[key][1]:bins[key]=p
    samples=np.array(list(bins.values()))
    def basis(p):
        x=(p[:,0]-mid[0])/half[0];z=(p[:,2]-mid[2])/half[2]
        return np.c_[np.ones(len(p)),z,z*z,z**3,z**4,x*x,x*x*z,x*x*z*z,x**4]
    design=basis(samples);weight=np.ones(len(samples))
    for _ in range(6):
        coeff=np.linalg.lstsq(design*weight[:,None],samples[:,1]*weight,rcond=None)[0]
        weight=np.where(samples[:,1]>design@coeff,.85,.15)**.5
    vertices=np.array(vertices)
    vertices[:,1]=basis(vertices)@coeff
    fitted=bpy.data.meshes.new(mesh.name+' continuous surface')
    fitted.from_pydata(vertices.tolist(),[],[tuple(reversed(f)) for f in faces])
    for face in fitted.polygons:face.use_smooth=True
    obj.data=fitted;mesh=fitted
    # Rear projection gives an undistorted, non-overlapping chart on the shell.
    coords = np.array([v.co[:] for v in mesh.vertices])
    lo, hi = coords.min(axis=0), coords.max(axis=0)
    uv = mesh.uv_layers.new(name='Rear panel UV')
    for face in mesh.polygons:
        for loop_id in face.loop_indices:
            co = mesh.vertices[mesh.loops[loop_id].vertex_index].co
            uv.data[loop_id].uv = (.03 + .94*(co.x-lo[0])/(hi[0]-lo[0]),
                                    .03 + .94*(co.z-lo[2])/(hi[2]-lo[2]))

    material = bpy.data.materials['Body_Color'].copy()
    material.name = ('Rear_Vent_Texture' if obj.name.startswith('Back Vent') else 'Engine_Cover_Texture')
    mesh.materials.append(material)
    shader = next(n for n in material.node_tree.nodes if n.type == 'BSDF_PRINCIPLED')
    node = material.node_tree.nodes.new('ShaderNodeTexImage')
    material.node_tree.nodes.active = node
    stem = 'rear-vent' if obj.name.startswith('Back Vent') else 'engine-cover'
    texture_dir = Path(root)/'textures'
    texture_dir.mkdir(exist_ok=True)
    width, height = (512, 128) if stem == 'rear-vent' else (512, 512)

    scene.render.engine = 'CYCLES'
    scene.cycles.device = 'CPU'
    scene.cycles.samples = 48
    scene.render.bake.use_selected_to_active = True
    scene.render.bake.use_clear = True
    scene.render.bake.cage_extrusion = .12
    scene.render.bake.max_ray_distance = .35
    scene.render.bake.margin = 16
    old_visibility = {o: o.hide_render for o in scene.objects}
    for other in scene.objects:
        other.hide_render = other not in {obj,reference}
    reference.hide_render = False
    obj.hide_render = False
    bpy.ops.object.select_all(action='DESELECT')
    obj.select_set(True)
    reference.select_set(True)
    bpy.context.view_layer.objects.active = obj

    # Bake slot shadows into color only: clean painted detail without modeled
    # lips, tangent-map distortions, or additional shading texture lookups.
    ao = bpy.data.images.new(stem+' AO bake',width=width,height=height,alpha=False,float_buffer=True)
    ao.colorspace_settings.name = 'Non-Color'
    node.image = ao
    bpy.ops.object.bake(type='AO')
    values = np.empty(width*height*4,dtype=np.float32)
    ao.pixels.foreach_get(values)
    pixels = values.reshape(-1,4)
    ambient = np.clip(pixels[:,:3].mean(axis=1),0,1)
    paint = np.array(shader.inputs['Base Color'].default_value[:3])
    pixels[:,:3] = paint[None,:] * (.08+.92*ambient[:,None]**2)
    pixels[:,3] = 1
    # Both panels are symmetric. Reuse the right half to avoid stray left-side
    # projection artifacts where the original narrow slats overlap the cage.
    rgba=values.reshape(height,width,4)
    rgba[:,:width//2,:]=rgba[:,width//2:,:][:,::-1,:].copy()
    color = bpy.data.images.new(stem+' base color',width=width,height=height,alpha=False,float_buffer=True)
    color.pixels.foreach_set(values)
    color.filepath_raw = str(texture_dir/(stem+'-color.png'))
    color.file_format = 'PNG'
    color.save()
    color.pack()
    material.node_tree.links.new(node.outputs['Color'],shader.inputs['Base Color'])

    node.image = color

    for other, state in old_visibility.items():
        other.hide_render = state
    bpy.data.objects.remove(reference,do_unlink=True)
    bpy.data.images.remove(ao)
    bpy.ops.object.select_all(action='DESELECT')
    obj.select_set(True)
    bpy.context.view_layer.objects.active = obj
    mesh.calc_loop_triangles()
    return {'method':'smooth exterior shell, baked slot shadows in color texture',
            'after':len(mesh.loop_triangles),'texture_size':[width,height]}

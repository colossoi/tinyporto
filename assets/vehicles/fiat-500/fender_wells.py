"""Painted wheel-well liners fitted to the simplified body's open arch edges.

Blender coordinates: X across the car, Y along it, Z up. Each liner has a
recessed back panel and a curved arch return; the underside stays open.
"""
import bpy
import bmesh
from mathutils import Vector


def reduced_curve(points, tolerance):
    """Keep endpoints and curvature while eliminating nearly collinear edges."""
    if len(points) <= 2:
        return points
    axis = points[-1] - points[0]
    distances = []
    for point in points[1:-1]:
        t = max(0, min(1, (point - points[0]).dot(axis) / axis.length_squared))
        distances.append((point - points[0] - axis * t).length)
    distance = max(distances)
    if distance <= tolerance:
        return [points[0], points[-1]]
    split = distances.index(distance) + 1
    return reduced_curve(points[:split + 1], tolerance)[:-1] + reduced_curve(points[split:], tolerance)


def add_fender_wells():
    for obj in list(bpy.context.scene.objects):
        if obj.name.startswith('Wheel Well '):
            bpy.data.objects.remove(obj, do_unlink=True)
    body = next(o for o in bpy.context.scene.objects if o.name.startswith('Main Chassis_'))
    paint = body.data.materials[0]
    bm = bmesh.new()
    bm.from_mesh(body.data)
    bmesh.ops.transform(bm, matrix=body.matrix_world, verts=list(bm.verts))
    bmesh.ops.remove_doubles(bm, verts=list(bm.verts), dist=0.0001)
    boundary = {}
    for edge in bm.edges:
        if edge.is_boundary:
            a, b = edge.verts
            boundary.setdefault(a, []).append(b)
            boundary.setdefault(b, []).append(a)
    result = []
    for corner in ('FL', 'FR', 'BL', 'BR'):
        tire = next(o for o in bpy.context.scene.objects if o.name.startswith(corner + '-Tire_'))
        coords = [tire.matrix_world @ v.co for v in tire.data.vertices]
        low = Vector([min(p[a] for p in coords) for a in range(3)])
        high = Vector([max(p[a] for p in coords) for a in range(3)])
        center = (low + high) * 0.5
        radius = (high.z - low.z) * 0.5
        side = 1 if center.x > 0 else -1
        inside = min(abs(low.x), abs(high.x)) - 0.08
        target = center + Vector((0, 0, radius * 1.12))
        candidates = [v for v in boundary if side * v.co.x > inside + 0.02
                      and abs(v.co.y - center.y) < radius * 0.25
                      and radius < v.co.z - center.z < radius * 1.4]
        top = min(candidates, key=lambda v: (v.co - target).length_squared)
        assert len(boundary[top]) == 2

        def follow(first):
            previous, current = top, first
            path = []
            for _ in range(200):
                p = current.co
                if side * p.x < inside + 0.02 or abs(p.y - center.y) > radius * 1.30:
                    break
                path.append(p.copy())
                if p.z < center.z - radius * 0.17:
                    break
                choices = [v for v in boundary[current] if v != previous]
                assert len(choices) == 1, 'arch boundary must be a simple chain'
                previous, current = current, choices[0]
            return path

        a, b = [follow(v) for v in boundary[top]]
        curve = list(reversed(a)) + [top.co.copy()] + b
        if curve[0].y > curve[-1].y:
            curve.reverse()
        curve = reduced_curve(curve, 0.003)
        # A tiny radial overlap hides the seam between the sampled return and
        # the existing curved fender; it does not change the exterior silhouette.
        for p in curve:
            radial = Vector((0, p.y - center.y, p.z - center.z)).normalized()
            p += radial * 0.006
        n = len(curve)
        vertices = [tuple(p) for p in curve] + [(side * inside, p.y, p.z) for p in curve]
        faces = [(i, i + 1, n + i + 1, n + i) for i in range(n - 1)]
        faces.append(tuple(range(n, n * 2)))
        mesh = bpy.data.meshes.new(corner + ' painted wheel well')
        mesh.from_pydata(vertices, [], faces)
        mesh.materials.append(paint)
        mesh.update()
        liner = bpy.data.objects.new('Wheel Well ' + corner + '_Body Paint', mesh)
        bpy.context.collection.objects.link(liner)
        # Orient every surface into the cavity: the cap faces out toward the
        # tire, and the arch return faces inward toward the wheel axle.
        edit = bmesh.new(); edit.from_mesh(mesh)
        for face in edit.faces:
            mid = face.calc_center_median()
            desired = Vector((side, 0, 0)) if len(face.verts) > 4 else Vector((0, center.y - mid.y, center.z - mid.z))
            if face.normal.dot(desired) < 0:
                face.normal_flip()
            face.smooth = len(face.verts) == 4
        bmesh.ops.triangulate(edit, faces=list(edit.faces))
        edit.to_mesh(mesh); edit.free()
        mesh.update(); mesh.calc_loop_triangles()
        # The back wall is inboard of the complete tire, not through its tread.
        assert side * vertices[n][0] < min(abs(low.x), abs(high.x)) - 0.079
        assert len(mesh.loop_triangles) < 160
        result.append(dict(name=liner.name, before=0, after=len(mesh.loop_triangles),
                           method='painted arch return and recessed inner panel',
                           arch_points=n, tire_clearance_source_units=0.08))
    bm.free()
    return result

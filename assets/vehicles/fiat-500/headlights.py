"""Round headlight lenses and bezels with evenly spaced radial topology."""
import math

import bpy
import bmesh
import numpy as np
from mathutils import Vector


def components(mesh):
    bm=bmesh.new(); bm.from_mesh(mesh)
    unseen=set(bm.verts)
    result=[]
    while unseen:
        v=unseen.pop(); group=[v]; stack=[v]
        while stack:
            v=stack.pop()
            for edge in v.link_edges:
                n=edge.other_vert(v)
                if n in unseen:
                    unseen.remove(n); stack.append(n); group.append(n)
        result.append(np.array([v.co[:] for v in group]))
    bm.free()
    return sorted(result,key=lambda p:tuple(p.mean(axis=0).round(3)))


def round_headlights(obj, lens=False):
    vertices=[]; faces=[]
    for points in components(obj.data):
        center=points.mean(axis=0)
        _,basis=np.linalg.eigh(np.cov(points.T))
        axis=basis[:,0]
        if axis[1]>0: axis=-axis
        right=np.array([1.,0.,0.]); right-=axis*np.dot(right,axis); right/=np.linalg.norm(right)
        up=np.cross(axis,right)
        axial=(points-center)@axis
        radial=np.linalg.norm(points-center-axial[:,None]*axis,axis=1)
        low,high=float(axial.min()),float(axial.max())
        segments=48 if lens or center[2]>1.3 else 32
        radius=float(np.quantile(radial,.995))
        if lens:
            # Shallow ellipsoid, with the source lens diameter and forward depth.
            radius*=.985
            tip=len(vertices); vertices.append(tuple(center+high*axis))
            rings=[(low+(high-low)*math.cos(phi),radius*math.sin(phi))
                   for phi in [math.pi/8,math.pi/4,3*math.pi/8,math.pi/2]]
        elif center[2]>1.3:
            radius*=.98
            rings=[(high-.09,radius*.95),(high-.028,radius),
                   (high,radius*.94),(high-.005,radius*.81)]
        else:
            rings=[(high-.035,radius*.95),(high,radius*.96),(high-.01,radius*.72)]
        start=len(vertices)
        for depth,r in rings:
            for k in range(segments):
                angle=2*math.pi*k/segments
                p=center+depth*axis+r*(right*math.cos(angle)+up*math.sin(angle))
                vertices.append(tuple(p))
        if lens:
            for k in range(segments): faces.append((tip,start+k,start+(k+1)%segments))
        for ring in range(len(rings)-1):
            for k in range(segments):
                n=(k+1)%segments
                faces.append((start+ring*segments+k,start+(ring+1)*segments+k,
                              start+(ring+1)*segments+n,start+ring*segments+n))
        if not lens:
            # A simple reflector closes the hollow bezel behind the glass.
            reflector=len(vertices)
            vertices.append(tuple(center+(high-.06)*axis))
            inner=start+(len(rings)-1)*segments
            for k in range(segments):
                faces.append((inner+k,reflector,inner+(k+1)%segments))
    mesh=bpy.data.meshes.new(obj.data.name+' radial topology')
    mesh.from_pydata(vertices,[],faces)
    for mat in obj.data.materials:mesh.materials.append(mat)
    obj.data=mesh
    bm=bmesh.new();bm.from_mesh(mesh)
    bmesh.ops.recalc_face_normals(bm,faces=list(bm.faces))
    # Lenses are open convex surfaces; their normals face toward the front.
    if lens and sum(f.normal.y*f.calc_area() for f in bm.faces)>0:
        bmesh.ops.reverse_faces(bm,faces=list(bm.faces))
    bm.to_mesh(mesh);bm.free()
    for face in mesh.polygons:face.use_smooth=True
    mesh.update()

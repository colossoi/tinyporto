"""Bake the simplified GLB into Tinyporto's static indexed-mesh buffers.

Run with Python + numpy. No Blender or GLB loader is needed at game runtime.
Positions are Y-up metres, centered in X/Z, with tires at Y=0 and +Z forward.
The source GLB and its Sketchfab upload remain unchanged.
"""
import hashlib
import json
import struct
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent


def prepare():
    source = (ROOT / "fiat-500-simplified.glb").read_bytes()
    assert struct.unpack_from("<III", source) == (0x46546C67, 2, len(source))
    size, kind = struct.unpack_from("<II", source, 12)
    assert kind == 0x4E4F534A
    doc = json.loads(source[20:20 + size])
    offset = 20 + size
    size, kind = struct.unpack_from("<II", source, offset)
    assert kind == 0x004E4942
    data = source[offset + 8:offset + 8 + size]

    def accessor(index):
        a = doc["accessors"][index]
        view = doc["bufferViews"][a["bufferView"]]
        assert "sparse" not in a and not a.get("normalized", False)
        dtype = np.dtype({5123: "<u2", 5125: "<u4", 5126: "<f4"}[a["componentType"]])
        width = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}[a["type"]]
        stride = view.get("byteStride", width * dtype.itemsize)
        assert a.get("byteOffset", 0) + (a["count"] - 1) * stride + width * dtype.itemsize <= view["byteLength"]
        result = np.ndarray((a["count"], width), dtype, buffer=data,
                            offset=view.get("byteOffset", 0) + a.get("byteOffset", 0),
                            strides=(stride, dtype.itemsize)).copy()
        assert np.isfinite(result).all()
        return result

    parts = []

    def visit(index, parent):
        node = doc["nodes"][index]
        if "matrix" in node:
            matrix = np.array(node["matrix"]).reshape(4, 4).T
        else:
            x, y, z, w = node.get("rotation", [0, 0, 0, 1])
            rotation = np.array([
                [1 - 2*y*y - 2*z*z, 2*x*y - 2*z*w, 2*x*z + 2*y*w],
                [2*x*y + 2*z*w, 1 - 2*x*x - 2*z*z, 2*y*z - 2*x*w],
                [2*x*z - 2*y*w, 2*y*z + 2*x*w, 1 - 2*x*x - 2*y*y]])
            matrix = np.eye(4)
            matrix[:3, :3] = rotation @ np.diag(node.get("scale", [1, 1, 1]))
            matrix[:3, 3] = node.get("translation", [0, 0, 0])
        world = parent @ matrix
        if "mesh" in node:
            mesh = doc["meshes"][node["mesh"]]
            for primitive in mesh["primitives"]:
                assert primitive.get("mode", 4) == 4
                attr = primitive["attributes"]
                position = accessor(attr["POSITION"]) @ world[:3, :3].T + world[:3, 3]
                normal = accessor(attr["NORMAL"]) @ np.linalg.inv(world[:3, :3])
                normal /= np.linalg.norm(normal, axis=1)[:, None]
                indices = accessor(primitive["indices"]).reshape(-1, 3)
                assert indices.max() < len(position)
                if np.linalg.det(world[:3, :3]) < 0:
                    indices = indices[:, [0, 2, 1]]
                named_material = doc["materials"][primitive["material"]]
                material = named_material["pbrMetallicRoughness"]
                color = material.get("baseColorFactor", [1, 1, 1, 1])
                texture = 0
                uv = np.zeros((len(position), 2))
                if "baseColorTexture" in material:
                    info = material["baseColorTexture"]
                    assert info.get("texCoord", 0) == 0 and "extensions" not in info
                    texture = {"Rear_Vent_Texture": 1, "Engine_Cover_Texture": 2}[named_material["name"]]
                    uv = accessor(attr["TEXCOORD_0"])
                vertices = np.zeros((len(position), 16), dtype="<f4")
                vertices[:, :3] = position
                vertices[:, 3] = material.get("roughnessFactor", 1)
                vertices[:, 4:7] = normal
                vertices[:, 7] = texture
                vertices[:, 8:12] = color
                vertices[:, 12:14] = uv
                parts.append((mesh["name"], vertices, indices))
        for child in node.get("children", []):
            visit(child, world)

    for node in doc["scenes"][doc.get("scene", 0)]["nodes"]:
        visit(node, np.eye(4))
    vertices = np.concatenate([v for _, v, _ in parts])
    low, high = vertices[:, :3].min(axis=0), vertices[:, :3].max(axis=0)
    scale = 3.0 / float(high[2] - low[2])
    center = (low + high) * 0.5
    center[1] = low[1]
    vertices[:, :3] = (vertices[:, :3] - center) * scale
    indices, start = [], 0
    for name, part, faces in parts:
        indices.append(faces.astype("<u4") + start)
        points = vertices[start:start + len(part), :3]
        print(name, len(faces), points.min(axis=0).round(3), points.max(axis=0).round(3))
        start += len(part)
    indices = np.concatenate(indices)
    assert np.isfinite(vertices).all()
    assert np.allclose(np.linalg.norm(vertices[:, 4:7], axis=1), 1, atol=0.001)
    expected = json.loads((ROOT / "simplification-report.json").read_text())["output_triangles"]
    assert len(indices) == expected and indices.max() < len(vertices)
    triangles = vertices[indices, :3]
    assert np.all(np.linalg.norm(np.cross(triangles[:, 1] - triangles[:, 0],
                                          triangles[:, 2] - triangles[:, 0]), axis=1) > 1e-12)
    output = ROOT / "scene"
    output.mkdir(exist_ok=True)
    (output / "vertices.bin").write_bytes(vertices.tobytes())
    (output / "indices.bin").write_bytes(indices.tobytes())
    (output / "mesh.wyn").write_text(
        "-- Generated by prepare_scene.py; counts describe the adjacent buffers.\n"
        f"def FIAT_INDEX_COUNT: u32 = {indices.size}u32\n")
    report = dict(source="fiat-500-simplified.glb", source_sha256=hashlib.sha256(source).hexdigest(),
                  triangles=len(indices), vertices=len(vertices), stride_bytes=64,
                  scale=scale, bounds=[vertices[:, :3].min(axis=0).tolist(), vertices[:, :3].max(axis=0).tolist()])
    (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    prepare()

#!/usr/bin/env python3
"""Static XMF/CMF919 -> Blender OBJ, with explicit centering/size metadata.

Default conversion preserves source size/position. OBJ is Y-up for Blender's default OBJ importer. Blender's import
conversion restores the native Z-up orientation. --z-up instead requires Z-up
in the importing program. --fit optionally centres/fits the mesh for a standalone preview.
Only the first UV set is exported. Bones/morph animation stay in the source.
Standard library only. Importing this module never opens dialogs or writes files.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import struct
import sys
import xml.etree.ElementTree as ET


class Reader:
    def __init__(self, data):
        self.data, self.at = data, 0

    def take(self, count):
        end = self.at + count
        if count < 0 or end > len(self.data):
            raise ValueError("Truncated binary CMF")
        result, self.at = self.data[self.at:end], end
        return result

    def integer(self):
        return struct.unpack('<i', self.take(4))[0]

    def count(self, maximum):
        count = self.integer()
        if not 0 <= count <= maximum:
            raise ValueError(f"Invalid CMF count {count}")
        return count

    def floats(self, count):
        result = struct.unpack('<' + 'f' * count, self.take(4 * count))
        if not all(math.isfinite(v) for v in result):
            raise ValueError("Non-finite CMF value")
        return result


def parse_cmf(data):
    r = Reader(data)
    if r.take(4) != b'CMF\0' or r.integer() != 919:
        raise ValueError("Binary import supports uncompressed CMF version 919")
    meshes, total = [], 0
    for _ in range(r.count(64)):
        material = r.integer()
        nv, nf = r.count(200_000), r.count(400_000)
        _lod, springs, maps, morphs = r.count(200_000), r.count(200_000), r.count(8), r.count(256)
        total += nv
        if total > 200_000:
            raise ValueError("Mesh exceeds 200,000 vertices")
        if springs:
            raise ValueError("CMF spring meshes are not supported by this converter")
        vertices, normals, uvs = [], [], []
        for _ in range(nv):
            vertices.append(r.floats(3))
            normals.append(r.floats(3))
            r.floats(3)  # colour
            r.take(8)    # collapse ID / face count
            coords = [r.floats(2) for _ in range(maps)]
            uvs.append(coords[0] if coords else (0.0, 0.0))
            for _ in range(r.count(64)):
                if r.integer() < 0:
                    raise ValueError("Negative bone ID")
                if r.floats(1)[0] < 0:
                    raise ValueError("Negative skin weight")
        # The seventh submesh integer is NUMMORPHS, not an unknown first-vertex
        # prefix. Sparse morph records occur BEFORE faces, even in a static OBJ.
        for _ in range(morphs):
            r.take(r.count(4096)).decode('utf-8')
            index, previous = r.integer(), -1
            while 0 <= index < nv:
                if index <= previous:
                    raise ValueError("Unsorted/repeated sparse morph vertex")
                previous = index
                r.floats(6 + maps * 2)
                index = r.integer()
        faces = [tuple(r.integer() for _ in range(3)) for _ in range(nf)]
        if any(i < 0 or i >= nv for face in faces for i in face):
            raise ValueError("Face index outside the submesh")
        meshes.append(dict(vertices=vertices, normals=normals, uvs=uvs, faces=faces, material=material))
    if r.at != len(data):
        raise ValueError("CMF contains unparsed trailing bytes")
    return meshes, 919


def values(element, name, length, default=None):
    text = element.findtext(name)
    if text is None and default is not None:
        return default
    if text is None:
        raise ValueError(f"Missing {name}")
    result = tuple(float(x) for x in text.split())
    if len(result) != length or not all(math.isfinite(x) for x in result):
        raise ValueError(f"Invalid {name}")
    return result


def parse_xmf(data):
    text = data.decode('utf-8-sig') if isinstance(data, bytes) else data.lstrip('\ufeff')
    text = re.sub(r'^\s*<\?xml[^?]*\?>', '', text).strip()
    root = ET.fromstring('<ROOT>' + text + '</ROOT>')
    mesh = root.find('.//MESH')
    if mesh is None:
        raise ValueError("No MESH in the XML")
    header = root.find('.//HEADER')
    version = int(header.get('VERSION', '919')) if header is not None else 919
    result, total = [], 0
    for part in mesh.findall('SUBMESH'):
        records = part.findall('VERTEX')
        nv = int(part.get('NUMVERTICES', len(records)))
        if nv != len(records) or not 0 <= nv <= 200_000:
            raise ValueError("Vertex count mismatch/out of bounds")
        total += nv
        if total > 200_000:
            raise ValueError("Mesh exceeds 200,000 vertices")
        by_id = {int(v.get('ID', i)): v for i, v in enumerate(records)}
        if set(by_id) != set(range(nv)):
            raise ValueError("Missing/duplicate/out-of-range vertex ID")
        vertices = [values(by_id[i], 'POS', 3) for i in range(nv)]
        normals = [values(by_id[i], 'NORM', 3) for i in range(nv)]
        uvs = [values(by_id[i], 'TEXCOORD', 2, (0.0, 0.0)) for i in range(nv)]
        faces = [tuple(int(i) for i in f.get('VERTEXID', '').split()) for f in part.findall('FACE')]
        if len(faces) != int(part.get('NUMFACES', len(faces))):
            raise ValueError("Face count mismatch")
        if any(len(f) != 3 or any(i < 0 or i >= nv for i in f) for f in faces):
            raise ValueError("Invalid triangular face")
        result.append(dict(vertices=vertices, normals=normals, uvs=uvs, faces=faces, material=int(part.get('MATERIAL', '0'))))
    if not result or not total:
        raise ValueError("No mesh vertices")
    return result, version


def load(path):
    path = Path(path)
    if path.stat().st_size > 64 * 1024 * 1024:
        raise ValueError("Input exceeds 64 MB")
    data = path.read_bytes()
    return (parse_cmf(data) if data.startswith(b'CMF\0') else parse_xmf(data)), data


def coordinate(v, center, scale, y_up):
    x, y, z = ((v[i] - center[i]) * scale for i in range(3))
    return (x, z, -y) if y_up else (x, y, z)


def export(path, output_dir, size=2.0, native=True, y_up=True, overwrite=False, keep_unused=True):
    if not math.isfinite(size) or size <= 0:
        raise ValueError("Size must be a positive finite number")
    (meshes, version), source = load(path)
    source_vertex_count = sum(len(m['vertices']) for m in meshes)
    mappings, compacted = [], []
    for source_index, mesh in enumerate(meshes):
        ids = list(range(len(mesh['vertices']))) if keep_unused else sorted({i for face in mesh['faces'] for i in face})
        if not ids:
            continue
        remap = {old: new for new, old in enumerate(ids)}
        new = dict(mesh)
        for key in ['vertices', 'normals', 'uvs']:
            new[key] = [mesh[key][i] for i in ids]
        new['faces'] = [tuple(remap[i] for i in face) for face in mesh['faces']]
        new['source_index'] = source_index
        compacted.append(new)
        mappings.append(dict(source_submesh=source_index, source_vertex_ids=ids))
    meshes = compacted
    vertices = [v for m in meshes for v in m['vertices']]
    if not vertices:
        raise ValueError("Empty geometry")
    low = [min(v[i] for v in vertices) for i in range(3)]
    high = [max(v[i] for v in vertices) for i in range(3)]
    extent = max(high[i] - low[i] for i in range(3))
    if extent <= 1e-12:
        raise ValueError("Degenerate mesh has no measurable size")
    center = [0.0] * 3 if native else [(low[i] + high[i]) / 2 for i in range(3)]
    scale = 1.0 if native else size / extent
    output_dir = Path(output_dir)
    obj = output_dir / (Path(path).stem + '.obj')
    metadata = obj.with_suffix('.transform.json')
    if not overwrite and (obj.exists() or metadata.exists()):
        raise FileExistsError(f"Output already exists: {obj}; use --overwrite intentionally")
    lines = ['# Static IMVU mesh for Blender', f'# Source version {version}',
             '# Y-up OBJ: use default Blender OBJ import axes' if y_up else '# Z-up OBJ: choose Z Up in your importer']
    offset = 1
    for index, mesh in enumerate(meshes):
        lines.append(f'o submesh_{mesh["source_index"]}_mat{mesh["material"]}')
        for v in mesh['vertices']:
            lines.append('v ' + ' '.join(f'{x:.9g}' for x in coordinate(v, center, scale, y_up)))
        for v in mesh['normals']:
            lines.append('vn ' + ' '.join(f'{x:.9g}' for x in coordinate(v, [0.0] * 3, 1.0, y_up)))
        for u, v in mesh['uvs']:
            lines.append(f'vt {u:.9g} {1.0-v:.9g}')
        lines.append(f'g submesh_{index}')
        for face in mesh['faces']:
            lines.append('f ' + ' '.join(f'{i+offset}/{i+offset}/{i+offset}' for i in face))
        offset += len(mesh['vertices'])
    info = dict(source=str(Path(path).resolve()), source_sha256=hashlib.sha256(source).hexdigest(),
                source_version=version, source_bounds=[low, high], center_removed=center,
                uniform_scale=scale, output_axes='Y-up: (x,z,-y)' if y_up else 'native Z-up',
                normalized=not native, normalized_largest_dimension=size if not native else None,
                source_vertex_count=source_vertex_count, removed_unused_vertices=source_vertex_count-len(vertices), vertex_mapping=mappings,
                vertices=len(vertices), faces=sum(len(m['faces']) for m in meshes), submeshes=len(meshes),
                note='Static base mesh only. OBJ does not preserve skeleton/morph animation. Center/scale metadata is recorded, not automatically reapplied on re-export.')
    output_dir.mkdir(parents=True, exist_ok=True)
    obj.write_text('\n'.join(lines) + '\n', encoding='utf-8')
    metadata.write_text(json.dumps(info, indent=2) + '\n', encoding='utf-8')
    return obj, info


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('files', nargs='*', type=Path)
    parser.add_argument('--output-dir', type=Path, default=Path(__file__).resolve().parent / 'obj_export')
    parser.add_argument('--size', type=float, default=2.0, help='Fit largest dimension to this many Blender scene units (default: 2)')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--native', action='store_true', help='Keep source size/position (the default)')
    mode.add_argument('--fit', action='store_true', help='Optional standalone preview: centre and fit to --size')
    parser.add_argument('--z-up', action='store_true', help='Write native Z-up coordinates; change importer Up Axis accordingly')
    parser.add_argument('--overwrite', action='store_true')
    loose = parser.add_mutually_exclusive_group()
    loose.add_argument('--keep-unused', action='store_true', help='Retain loose vertices (default unless --fit)')
    loose.add_argument('--clean-unused', action='store_true', help='Omit unreferenced vertices without changing coordinates')
    args = parser.parse_args(argv)
    if not args.files:
        entered = input('Paste an XMF/CMF path (or leave empty to exit): ').strip().strip('"')
        if not entered:
            return 0
        args.files = [Path(entered)]
    failed = False
    for path in args.files:
        try:
            obj, info = export(path, args.output_dir, args.size, not args.fit, not args.z_up, args.overwrite, args.keep_unused or not (args.fit or args.clean_unused))
            print(f'{path.name} -> {obj} ({info["vertices"]} vertices, {info["faces"]} faces)')
        except (OSError, ValueError, ET.ParseError, UnicodeError) as error:
            print(f'{path}: {error}', file=sys.stderr)
            failed = True
    return int(failed)


if __name__ == '__main__':
    raise SystemExit(main())

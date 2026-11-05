#!/usr/bin/env python3
"""Drop XMF/CMF files on the launcher to create static toolkit-scale .blend files."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def find_blender(explicit=None):
    choices = [explicit, os.environ.get('BLENDER_EXE'), shutil.which('blender')]
    base = Path(os.environ.get('ProgramFiles', r'C:\Program Files')) / 'Blender Foundation'
    choices += sorted(base.glob('Blender */blender.exe'), reverse=True)
    for candidate in choices:
        if candidate and Path(candidate).is_file():
            return str(Path(candidate).resolve())
    raise ValueError('Blender was not found. Set BLENDER_EXE to your blender.exe path.')


def worker(manifest):
    import bpy
    from mathutils import Quaternion, Vector
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from xmf_to_obj_blender import parse_cmf, parse_xmf
    jobs = json.loads(Path(manifest).read_text(encoding='utf-8'))
    failed = False
    for job in jobs:
        try:
            source, output = Path(job['source']), Path(job['output'])
            if output.exists():
                raise ValueError(f'Output exists: {output}')
            if source.stat().st_size > 64 * 1024 * 1024:
                raise ValueError('Source exceeds 64 MiB')
            raw = source.read_bytes()
            meshes, version = parse_cmf(raw) if raw.startswith(b'CMF\0') else parse_xmf(raw)
            bpy.ops.wm.read_factory_settings(use_empty=True)
            scene = bpy.context.scene
            scene.unit_settings.system = 'METRIC'
            points = []
            for i, part in enumerate(meshes):
                # Both native IMVU and Blender use Z-up. Direct mesh creation needs no OBJ axis rotation.
                vertices = [tuple(c * .01 for c in v) for v in part['vertices']]
                mesh = bpy.data.meshes.new(f'Submesh {i}')
                mesh.from_pydata(vertices, [], part['faces'])
                mesh.update()
                uv = mesh.uv_layers.new(name='UVMap')
                for loop in mesh.loops:
                    u, v = part['uvs'][loop.vertex_index]
                    uv.data[loop.index].uv = (u, 1-v)
                if part['normals'] and len(part['normals']) == len(vertices):
                    mesh.normals_split_custom_set_from_vertices(part['normals'])
                obj = bpy.data.objects.new(f'{source.stem}_submesh_{i}_mat{part["material"]}', mesh)
                scene.collection.objects.link(obj)
                obj['source_material_id'] = part['material']
                obj['source_submesh'] = i
                obj.select_set(True)
                bpy.context.view_layer.objects.active = obj
                points.extend(vertices)
            if not points:
                raise ValueError('Mesh contains no vertices')
            low = Vector([min(v[i] for v in points) for i in range(3)])
            high = Vector([max(v[i] for v in points) for i in range(3)])
            center = (low+high)*.5
            for screen in bpy.data.screens:
                for area in screen.areas:
                    if area.type == 'VIEW_3D':
                        space = area.spaces.active
                        space.region_3d.view_location = center
                        space.region_3d.view_distance = max((high-low).length*1.25, .1)
                        space.region_3d.view_rotation = Quaternion((1,0,0), 1.5707963267948966)
                        space.clip_end = max(1000, high.length*4)
            scene['source_xmf'] = str(source)
            scene['source_version'] = version
            scene['imvu_to_toolkit_scale'] = .01
            scene['conversion_note'] = 'Static geometry and first UV set. No centering. Textures, transparency, skeleton, weights and morphs are not imported.'
            output.parent.mkdir(parents=True, exist_ok=True)
            bpy.ops.wm.save_as_mainfile(filepath=str(output))
            print(f'CREATED: {output}', flush=True)
        except Exception as error:
            failed = True
            print(f'FAILED: {job["source"]}: {error}', flush=True)
    if failed:
        raise RuntimeError('One or more XMF conversions failed; see FAILED messages above.')


def main():
    if '--blend-worker' in sys.argv:
        worker(sys.argv[sys.argv.index('--blend-worker')+1])
        return 0
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('files', nargs='*', type=Path)
    parser.add_argument('--blender', help='Optional blender.exe path')
    parser.add_argument('--output-dir', type=Path, default=Path(__file__).resolve().parent/'blend_export')
    args = parser.parse_args()
    if not args.files:
        path = input('Paste an XMF path (or drop files on the .cmd launcher): ').strip().strip('"')
        if not path:
            return 0
        args.files = [Path(path)]
    try:
        blender = find_blender(args.blender)
        jobs, reserved = [], set()
        for source in args.files:
            source = source.resolve(strict=True)
            if source.suffix.lower() not in ('.xmf', '.cmf'):
                raise ValueError(f'Expected an XMF/CMF file: {source}')
            output = args.output_dir.resolve()/(source.stem+'.blend')
            counter = 2
            while output.exists() or str(output) in reserved:
                output = args.output_dir.resolve()/f'{source.stem}_{counter}.blend'
                counter += 1
            reserved.add(str(output))
            jobs.append(dict(source=str(source), output=str(output)))
        with tempfile.TemporaryDirectory(prefix='xmf-blend-') as temporary:
            manifest = Path(temporary)/'jobs.json'
            manifest.write_text(json.dumps(jobs), encoding='utf-8')
            result = subprocess.run([blender, '--background', '--factory-startup', '--disable-autoexec', '--python-exit-code', '1', '--python', str(Path(__file__).resolve()), '--', '--blend-worker', str(manifest)])
            if result.returncode or not all(Path(j['output']).is_file() for j in jobs):
                return 1
        print(f'\nSaved Blender files in: {args.output_dir.resolve()}')
        return 0
    except (OSError, ValueError) as error:
        print(f'Conversion failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
"""Drop XML XAF files on the launcher to open a pose/animation as a Blender armature."""
import argparse
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET


def build(source, skeleton_path, output, fps=30):
    import bpy
    from mathutils import Matrix, Quaternion, Vector
    from imvu_pose import skeleton, animation
    bones=skeleton(skeleton_path);clip=animation(source,bones)
    frames=math.ceil(clip['duration']*fps)
    if frames>100000 or (frames+2)*len(clip['tracks'])>200000:
        raise ValueError('Animation is too large to bake; shorten it or use a lower --fps')
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene=bpy.context.scene;scene.render.fps=fps
    scene.unit_settings.system='METRIC'
    data=bpy.data.armatures.new('IMVU source skeleton')
    arm=bpy.data.objects.new('IMVU pose',data);scene.collection.objects.link(arm)
    arm.show_in_front=True;data.display_type='STICK'
    arm.select_set(True);bpy.context.view_layer.objects.active=arm
    def matrix(position,rotation):
        m=Quaternion(rotation).to_matrix().to_4x4();m.translation=Vector(position)*.01;return m
    local=[matrix(b['position'],b['rotation']) for b in bones]
    rest={}
    def world(i,locals,out):
        if i not in out:
            p=bones[i]['parent'];out[i]=(world(p,locals,out)@locals[i]) if p>=0 else locals[i].copy()
        return out[i]
    for i in range(len(bones)):world(i,local,rest)
    names=[f'{b["id"]:03d}_{b["name"]}' for b in bones]
    bpy.ops.object.mode_set(mode='EDIT')
    for i,b in enumerate(bones):
        e=data.edit_bones.new(names[i]);e.head=(0,0,0);e.tail=(0,.15,0);e.matrix=rest[i]
    for i,b in enumerate(bones):
        if b['parent']>=0:data.edit_bones[names[i]].parent=data.edit_bones[names[b['parent']]]
    bpy.ops.object.mode_set(mode='OBJECT')
    relative=[]
    for i,b in enumerate(bones):
        bone=data.bones[names[i]];bone['imvu_bone_id']=i;bone['imvu_source_name']=b['name']
        relative.append((data.bones[names[b['parent']]].matrix_local.inverted()@bone.matrix_local) if b['parent']>=0 else bone.matrix_local.copy())
        arm.pose.bones[names[i]].rotation_mode='QUATERNION'
        # Edit bones may quantize near-degenerate roll orientations. Compensate against
        # their actual stored rest matrices, including bones with no animation track.
        arm.pose.bones[names[i]].matrix_basis=relative[i].inverted()@local[i]
    # Include every source key time as well as integer frames. Bake source slerp at 30fps.
    times=sorted({0.,clip['duration']}|{min(i/fps,clip['duration']) for i in range(frames+1)}|{k['time'] for keys in clip['tracks'].values() for k in keys})
    if len(times)*len(clip['tracks'])>200000:raise ValueError('Too many baked bone keys')
    previous={}
    def sample(keys,time):
        if time<=keys[0]['time']:return keys[0]
        for a,b in zip(keys,keys[1:]):
            if time<=b['time']:
                f=(time-a['time'])/(b['time']-a['time'])
                return dict(position=Vector(a['position']).lerp(Vector(b['position']),f),rotation=Quaternion(a['rotation']).slerp(Quaternion(b['rotation']),f))
        return keys[-1]
    for time in times:
        for i,keys in clip['tracks'].items():
            k=sample(keys,time);basis=relative[i].inverted()@matrix(k['position'],k['rotation'])
            pb=arm.pose.bones[names[i]];pb.matrix_basis=basis
            if i in previous and pb.rotation_quaternion.dot(previous[i])<0:pb.rotation_quaternion.negate()
            previous[i]=pb.rotation_quaternion.copy()
            pb.keyframe_insert('location',frame=1+time*fps,group=names[i])
            pb.keyframe_insert('rotation_quaternion',frame=1+time*fps,group=names[i])
    action=arm.animation_data.action;action.name=Path(source).stem
    # Blender 4.4+ layered Actions and legacy Actions have different curve accessors.
    curves=[]
    if hasattr(action,'fcurves'):curves.extend(action.fcurves)
    for layer in getattr(action,'layers',[]):
        for strip in layer.strips:
            for bag in getattr(strip,'channelbags',[]):curves.extend(bag.fcurves)
    for curve in curves:
        for key in curve.keyframe_points:key.interpolation='LINEAR'
    scene.frame_start=1;scene.frame_end=1+frames;scene.frame_set(1)
    arm['source_xaf']=str(source);arm['source_xsf']=str(skeleton_path);arm['imvu_to_toolkit_scale']=.01
    scene['conversion_note']='Source skeleton, not toolkit control rig. Static pose or animation baked at integer frames plus source key times. Subframe interpolation approximates source slerp. No mesh or retargeting.'
    scene['source_duration_seconds']=clip['duration']
    bpy.context.view_layer.update()
    positions=[pb.matrix.translation for pb in arm.pose.bones]
    low=Vector([min(p[j] for p in positions) for j in range(3)]);high=Vector([max(p[j] for p in positions) for j in range(3)])
    for screen in bpy.data.screens:
        for area in screen.areas:
            if area.type=='VIEW_3D':
                area.spaces.active.region_3d.view_location=(low+high)*.5
                area.spaces.active.region_3d.view_distance=max((high-low).length,1)*1.3
                area.spaces.active.region_3d.view_rotation=Quaternion((1,0,0),math.pi/2)
    bpy.ops.object.mode_set(mode='POSE')
    output=Path(output)
    if output.exists():raise ValueError(f'Output exists: {output}')
    output.parent.mkdir(parents=True,exist_ok=True)
    bpy.ops.wm.save_as_mainfile(filepath=str(output))
    print(f'CREATED: {output} ({len(bones)} bones, {len(clip["tracks"])} tracks, {clip["duration"]:g}s)',flush=True)


def main():
    sys.path.insert(0,str(Path(__file__).resolve().parent))
    if '--pose-worker' in sys.argv:
        jobs=json.loads(Path(sys.argv[sys.argv.index('--pose-worker')+1]).read_text())
        for job in jobs:build(**job)
        return 0
    from xmf_to_blend import find_blender
    from imvu_pose import skeleton, animation
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('files',nargs='*',type=Path)
    parser.add_argument('--skeleton',type=Path,default=Path(__file__).resolve().parent/'original-avatar.xsf')
    parser.add_argument('--output-dir',type=Path,default=Path(__file__).resolve().parent/'pose_blend_export')
    parser.add_argument('--fps',type=int,default=30)
    parser.add_argument('--blender')
    args=parser.parse_args()
    if not args.files:
        value=input('Paste an XML XAF path: ').strip().strip('"')
        if not value:return 0
        args.files=[Path(value)]
    if not 1<=args.fps<=120:parser.error('--fps must be 1–120')
    try:
        bones=skeleton(args.skeleton);jobs=[];reserved=set()
        for source in args.files:
            source=source.resolve(strict=True);animation(source,bones)
            output=args.output_dir.resolve()/(source.stem+'.blend');n=2
            while output.exists() or str(output) in reserved:
                output=args.output_dir.resolve()/f'{source.stem}_{n}.blend';n+=1
            reserved.add(str(output));jobs.append(dict(source=str(source),skeleton_path=str(args.skeleton.resolve()),output=str(output),fps=args.fps))
        with tempfile.TemporaryDirectory(prefix='xaf-blend-') as temp:
            manifest=Path(temp)/'jobs.json';manifest.write_text(json.dumps(jobs))
            result=subprocess.run([find_blender(args.blender),'--background','--factory-startup','--disable-autoexec','--python-exit-code','1','--python',str(Path(__file__).resolve()),'--','--pose-worker',str(manifest)])
        return int(result.returncode!=0 or not all(Path(j['output']).is_file() for j in jobs))
    except (ValueError,OSError,UnicodeError,ET.ParseError) as error:
        print(f'Conversion failed: {error}',file=sys.stderr);return 1


if __name__=='__main__':raise SystemExit(main())

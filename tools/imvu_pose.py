"""Bounded XML XSF/XAF reader shared by the Blender pose bridge. No Blender dependency."""
import math
from pathlib import Path
import re
import xml.etree.ElementTree as ET


def xml(path, magic):
    path = Path(path)
    if path.stat().st_size > 16*1024*1024:
        raise ValueError('Pose/skeleton exceeds 16 MiB')
    text = path.read_text(encoding='utf-8-sig').strip()
    if '<!DOCTYPE' in text.upper() or '<!ENTITY' in text.upper():
        raise ValueError('XML entity declarations are not supported')
    text = re.sub(r'^\s*<\?xml[^?]*\?>', '', text).strip()
    root = ET.fromstring('<ROOT>'+text+'</ROOT>')
    header = root.find('HEADER')
    if header is not None:
        if header.get('MAGIC') != magic or not 699 <= int(header.get('VERSION','0')) <= 919:
            raise ValueError(f'Expected a supported XML {magic} header')
    elif magic == 'XAF':
        raise ValueError('Missing XAF header; binary CAF is not supported')
    return root


def numbers(text, count):
    values = tuple(float(x) for x in (text or '').split())
    if len(values) != count or not all(math.isfinite(x) for x in values):
        raise ValueError(f'Expected {count} finite coordinates')
    return values


def rotation(text):
    x,y,z,w = numbers(text,4)
    # Cal3D file W has the opposite sign from the app/Blender quaternion convention.
    norm = math.hypot(x,y,z,w)
    if norm < 1e-12:
        raise ValueError('Zero rotation quaternion')
    return tuple(v/norm for v in (-w,x,y,z))  # Blender WXYZ


def skeleton(path):
    node = xml(path,'XSF').find('SKELETON')
    if node is None:
        raise ValueError('Missing SKELETON')
    bones = []
    for e in node.findall('BONE'):
        bones.append(dict(id=int(e.get('ID','-1')),name=e.get('NAME',''),parent=int(e.findtext('PARENTID','-2')),position=numbers(e.findtext('TRANSLATION'),3),rotation=rotation(e.findtext('ROTATION'))))
    bones.sort(key=lambda b:b['id'])
    if not 1 <= len(bones) <= 512 or len(bones) != int(node.get('NUMBONES','-1')):
        raise ValueError('Skeleton count mismatch/out of bounds')
    for i,b in enumerate(bones):
        if b['id'] != i or not b['name'] or len(b['name'])>256 or b['parent'] < -1 or b['parent']>=len(bones):
            raise ValueError('Invalid bone ID/name/parent')
        seen = set(); current=i
        while current>=0:
            if current in seen:
                raise ValueError('Skeleton parent cycle')
            seen.add(current);current=bones[current]['parent']
    return bones


def animation(path, bones):
    node=xml(path,'XAF').find('ANIMATION')
    if node is None:
        raise ValueError('Missing ANIMATION')
    duration=float(node.get('DURATION','nan'))
    if not math.isfinite(duration) or not 0<duration<=86400:
        raise ValueError('Invalid animation duration')
    tracks={};total=0
    for e in node.findall('TRACK'):
        bone=int(e.get('BONEID','-1'))
        if bone not in range(len(bones)) or bone in tracks:
            raise ValueError('Unknown or duplicate bone track')
        keys=[];previous=-1
        for key in e.findall('KEYFRAME'):
            time=float(key.get('TIME','nan'))
            if not math.isfinite(time) or not 0<=time<=duration+1e-5 or time<=previous:
                raise ValueError('Key times must increase within duration')
            previous=time
            translation=key.findtext('TRANSLATION')
            keys.append(dict(time=time,position=numbers(translation,3) if translation is not None else bones[bone]['position'],rotation=rotation(key.findtext('ROTATION'))))
            total+=1
            if total>200000:
                raise ValueError('Too many animation keys')
        if not keys or len(keys)!=int(e.get('NUMKEYFRAMES','-1')):
            raise ValueError('Key count mismatch')
        tracks[bone]=keys
    if not tracks or len(tracks)>512 or len(tracks)!=int(node.get('NUMTRACKS','-1')):
        raise ValueError('Track count mismatch')
    return dict(duration=duration,tracks=tracks)

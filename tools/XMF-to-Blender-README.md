# XMF to Blender — quick drag and drop

Drag one or more `.xmf` files onto **Drop XMF here to make Blender file.cmd**.

Open the resulting `.blend` files in the **blend_export** folder beside the launcher. The window prints their exact paths. Existing output files receive numbered names instead of being overwritten. You can also double-click the launcher and paste one file path.

Requires installed Python 3 and Blender. Blender is detected automatically in its standard Windows installation folder or PATH. An optional `BLENDER_EXE` environment variable can specify another installation.

Keep the launcher, `xmf_to_blend.py` and `xmf_to_obj_blender.py` together. Conversion runs Blender in the background and does not change your open Blender scene.

The result preserves shared placement, applies the fixed 0.01 native-IMVU-to-toolkit unit conversion, and creates Blender-native Z-up geometry directly. It does not recenter individual parts. The viewport is framed around the mesh without moving it. UVs and base normals are included.

This quick script exports static geometry only. It does not automatically load textures/opacity maps, armatures, bone weights or facial morphs. It is useful for opening/editing the mesh; it is not a complete rigged IMVU product round trip.

Optional command line:

    python xmf_to_blend.py "C:\path\Head.xmf" --output-dir "C:\path\output"

Original XMF files are read only. Tested with Blender 5.2.3 LTS on the supplied hair and earring meshes; XML/binary parsing uses the checked converter shared with the OBJ helper.

## Pose conversion

The separate Drop XAF here to make Blender pose.cmd creates a matching source armature and animation. It does not yet attach meshes from this converter or preserve their weights. See XAF-to-Blender-README.md.

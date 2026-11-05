# Poser to Blender

1. In HeadMaker's **Avatar Poser**, export the pose/animation as **XAF**.
2. Drag that XAF onto **Drop XAF here to make Blender pose.cmd**.
3. Open the `.blend` in **pose_blend_export** beside the launcher.
4. Blender opens in Pose Mode. Press Space to play, or scrub the timeline.

It creates an actual editable Blender armature using the app's matching 87-bone source skeleton. Translations use the same fixed 0.01 conversion as the mesh helper, with no recentering. Source bone IDs are preserved; names are prefixed with their IDs because the source has duplicate finger names. Animated and held poses both work. Existing outputs receive numbered filenames.

This first bridge imports the skeleton and animation, **not an avatar mesh, materials or the toolkit's IK controls**. It does not retarget to the toolkit rig or export edits back to XAF yet. For couple mode, convert Avatar-A.xaf and Avatar-B.xaf separately; these are separate files, not an automatically assembled Blender couple scene.

Requires Python and installed Blender. Keep these files together: launcher, xaf_to_blend.py, imvu_pose.py, xmf_to_blend.py, original-avatar.xsf. Blender is detected using the standard installation folder, PATH or BLENDER_EXE.

XML XAF only; binary CAF is currently unsupported. The bundled skeleton matches poses from this app. Bone IDs alone cannot prove compatibility with an unrelated skeleton. For another XML XSF use:

    python xaf_to_blend.py "pose.xaf" --skeleton "matching.xsf"

Optional `--fps 30`, `--output-dir "folder"`, `--blender "path\blender.exe"`. Duration and bake limits are checked; long dense animations may need a lower FPS or shorter clip. Source key times and integer frames are sampled using the source's translation interpolation and quaternion slerp. Blender keys use linear interpolation; arbitrary subframe rotations approximate source slerp, so this is not an exact continuous-time or reverse-conversion guarantee.

Tested in Blender 5.2.3 LTS by reopening saved files and comparing all 87 bone world matrices at 32 time samples, including elbow, fingers, translated/rotated root and pelvis, noninteger key times and quaternion sign changes. Maximum component error was below 0.000005. No IMVU runtime reimport or cross-version Blender certification is claimed.

Sources for Blender transform semantics:
- https://docs.blender.org/api/5.1/bpy.types.Bone.html
- https://docs.staging.blender.org/api/current/bpy.types.PoseBone.html

# XMF/CMF to Blender OBJ — faithful conversion by default

`xmf_to_obj_blender.py` is a standalone helper based on the supplied converter. Python3 standard library only. Original Downloads script is unchanged.

## Default: preserve size and position

```powershell
python xmf_to_obj_blender.py "C:\path\to\hair.xmf"
```

Default conversion preserves source coordinates/units and all vertices. It writes Y-up OBJ axes `(x,z,-y)` for Blender's default **Forward -Z / Up Y** import; Blender's importer restores the original Z-up orientation. No centring or normalising is performed. Related parts from the same native avatar coordinate system retain their alignment.

OBJ/transformJSON outputs are in `obj_export` beside the script unless `--output-dir` is supplied. Existing files are protected unless `--overwrite`. Drag source files onto the accompanying `.cmd` launcher, or run/double-click the script and paste a path.

## Options

- `--native`: explicit alias for the default preservation mode.
- `--z-up`: write native axes unchanged too; use Blender Forward Y / Up Z to preserve the native coordinate orientation. This is useful for byte-coordinate comparison with the original converter, not default Blender import settings.
- `--clean-unused`: omit vertices not referenced by any face, without changing size/position of retained vertices. Useful for the known legacy head eyebrow outlier atZ98,214.5. Default retains source data.
- `--fit`: opt-in standalone preview only. Centre the combined visible mesh and fit its largest dimension to2 scene units, or `--size N`. It omits unused vertices unless `--keep-unused` is supplied. This is not required for format decoding or native avatar alignment.
- `--keep-unused`: explicitly retain loose vertices, including when fitting.
- `--overwrite`: intentionally replace existing generated outputs.

Do not independently fit heads, hair and body parts that need to align as an avatar. Fitting is an optional presentation transform, not an IMVU unit conversion or repair of binary values.

The JSON sidecar records source bounds, centre/scale, axes, source hash and original vertex-ID mapping. Transforms are not automatically reversed when exporting edits from Blender.

## Format handling

Binary import supports investigated uncompressedCMF919, properly reading NUMMORPHS and skipping sparse morphs before faces. Static highest-detail LOD geometry is supported; spring meshes are rejected. XML accepts BOMs, declarations and HEADER/MESH fragments with checked vertex IDs. The first UV set and normals are exported; all parts are transformed together. OBJ is static geometry and does not transfer rig/morph animation or package material images.

Six Python regressions cover binary head morphs, explicit preview fitting, the unused legacy eyebrow outlier, default native preservation, BOM/XML fragments, malformed data and overwrite protection. The default change follows a direct comparison using supplied Hair.010.xmf: the original script's decoded coordinates/faces/normals/UVs matched the checked parser exactly; raw native offsets were already in the source. See `analysis/converter-comparison` for evidence.

## Direct Blender alternatives

For a ready-to-open .blend with fixed toolkit0.01 units and no OBJ axis settings, use Drop XMF here to make Blender file.cmd. For a source skeleton/animation from XML XAF, use Drop XAF here to make Blender pose.cmd. See the companion guides; neither currently transfers a complete skinned/textured IMVU product.

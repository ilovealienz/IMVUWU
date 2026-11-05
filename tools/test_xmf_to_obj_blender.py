import tempfile
import unittest
from pathlib import Path
import xmf_to_obj_blender as converter

ROOT = Path(__file__).resolve().parents[1]


class ConverterTests(unittest.TestCase):
    def test_actual_binary_head_morphs_are_skipped_before_faces(self):
        for name in ['female/Female04_Anime01_HeadMorph.xmf', 'male/Male03_Anime01_HeadMorphFile.xmf']:
            (meshes, version), _ = converter.load(ROOT / 'assets/poser' / name)
            self.assertEqual(version, 919)
            self.assertEqual([len(m['vertices']) for m in meshes], [959, 172, 66, 48])
            self.assertTrue(all(0 <= i < len(m['vertices']) for m in meshes for f in m['faces'] for i in f))

    def test_normalized_obj_is_centered_two_units_and_indices_are_valid(self):
        source = ROOT / 'assets/poser/female/Female04_Anime01_HeadMorph.xmf'
        with tempfile.TemporaryDirectory() as folder:
            obj, info = converter.export(source, folder, native=False, keep_unused=False)
            lines = obj.read_text().splitlines()
            vertices = [tuple(map(float, line.split()[1:])) for line in lines if line.startswith('v ')]
            extents = []
            for axis in range(3):
                low, high = min(v[axis] for v in vertices), max(v[axis] for v in vertices)
                self.assertAlmostEqual(low + high, 0.0, places=6)
                extents.append(high-low)
            self.assertAlmostEqual(max(extents), 2.0, places=6)
            self.assertGreater(info['center_removed'][2], 900)
            self.assertTrue(obj.with_suffix('.transform.json').exists())
            for line in lines:
                if line.startswith('f '):
                    for item in line.split()[1:]:
                        a, b, c = map(int, item.split('/'))
                        self.assertEqual(a, b)
                        self.assertEqual(b, c)
                        self.assertTrue(1 <= a <= len(vertices))

    def test_legacy_unused_eyebrow_outlier_does_not_distort_blender_bounds(self):
        source = ROOT / 'assets/NewExportHead6.xmf'
        with tempfile.TemporaryDirectory() as folder:
            obj, info = converter.export(source, folder, native=False, keep_unused=False)
            self.assertEqual(info['source_vertex_count'], 956)
            self.assertEqual(info['removed_unused_vertices'], 1)
            self.assertEqual(info['vertices'], 955)
            self.assertLess(info['source_bounds'][1][2], 1200)
            self.assertGreater(info['uniform_scale'], 0.005)
            self.assertNotIn(34, info['vertex_mapping'][3]['source_vertex_ids'])
            points = [tuple(map(float, l.split()[1:])) for l in obj.read_text().splitlines() if l.startswith('v ')]
            self.assertTrue(all(abs(n) <= 1.00001 for p in points for n in p))

    def test_native_mode_preserves_scale_and_offset(self):
        source = ROOT / 'assets/poser/female/Female04_Anime01_HeadMorph.xmf'
        (meshes, _), _ = converter.load(source)
        with tempfile.TemporaryDirectory() as folder:
            obj, info = converter.export(source, folder)
            first = next(line for line in obj.read_text().splitlines() if line.startswith('v '))
            values = list(map(float, first.split()[1:]))
            expected = converter.coordinate(meshes[0]['vertices'][0], [0, 0, 0], 1.0, True)
            self.assertEqual(info['uniform_scale'], 1.0)
            self.assertEqual(info['removed_unused_vertices'], 0)
            self.assertEqual(info['center_removed'], [0, 0, 0])
            for a, b in zip(values, expected):
                self.assertAlmostEqual(a, b, places=5)

    def test_xml_bom_declaration_and_sibling_roots(self):
        source = (ROOT / 'assets/NewExportHead6.xmf').read_text(encoding='utf-8-sig')
        source = converter.re.sub(r'^\s*<\?xml[^?]*\?>', '', source)
        meshes, _ = converter.parse_xmf('\ufeff<?xml version="1.0"?>\n' + source)
        self.assertEqual(sum(len(m['vertices']) for m in meshes), 956)

    def test_truncation_and_accidental_overwrite_are_rejected(self):
        source = ROOT / 'assets/poser/female/Female04_Anime01_HeadMorph.xmf'
        with self.assertRaises(ValueError):
            converter.parse_cmf(source.read_bytes()[:-1])
        with tempfile.TemporaryDirectory() as folder:
            converter.export(source, folder)
            with self.assertRaises(FileExistsError):
                converter.export(source, folder)


if __name__ == '__main__':
    unittest.main()

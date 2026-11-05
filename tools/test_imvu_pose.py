import tempfile
from pathlib import Path
import unittest
from imvu_pose import animation, skeleton

XSF='<HEADER MAGIC="XSF" VERSION="919"/><SKELETON NUMBONES="2"><BONE ID="0" NAME="same"><TRANSLATION>0 0 0</TRANSLATION><ROTATION>0 0 0 -1</ROTATION><PARENTID>-1</PARENTID></BONE><BONE ID="1" NAME="same"><TRANSLATION>0 100 0</TRANSLATION><ROTATION>0 0 0 -1</ROTATION><PARENTID>0</PARENTID></BONE></SKELETON>'
XAF='<HEADER MAGIC="XAF" VERSION="919"/><ANIMATION DURATION="1" NUMTRACKS="1"><TRACK BONEID="1" NUMKEYFRAMES="2"><KEYFRAME TIME="0"><ROTATION>0 0 0 -1</ROTATION></KEYFRAME><KEYFRAME TIME="1"><ROTATION>0 0 1 0</ROTATION></KEYFRAME></TRACK></ANIMATION>'


class PoseReaderTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.path=Path(self.temp.name)
    def read(self,text,kind):
        file=self.path/('file.'+kind);file.write_text(text,encoding='utf-8');return file
    def test_duplicate_names_keep_distinct_ids_and_rotation_only_uses_rest(self):
        bones=skeleton(self.read('\ufeff<?xml version="1.0"?>'+XSF,'xsf'))
        self.assertEqual([b['id'] for b in bones],[0,1])
        clip=animation(self.read(XAF,'xaf'),bones)
        self.assertEqual(clip['tracks'][1][0]['position'],(0,100,0))
        self.assertEqual(clip['tracks'][1][0]['rotation'],(1,0,0,0))
    def test_cycles_and_unknown_bones_rejected(self):
        with self.assertRaises(ValueError):skeleton(self.read(XSF.replace('<PARENTID>-1','<PARENTID>1'),'xsf'))
        bones=skeleton(self.read(XSF,'xsf'))
        with self.assertRaises(ValueError):animation(self.read(XAF.replace('BONEID="1"','BONEID="4"'),'xaf'),bones)
    def test_bad_quaternions_times_counts_and_entities_rejected(self):
        bones=skeleton(self.read(XSF,'xsf'))
        for text in (XAF.replace('0 0 0 -1','0 0 0 0'), XAF.replace('TIME="1"','TIME="nan"'), XAF.replace('TIME="1"','TIME="0"'), XAF.replace('NUMKEYFRAMES="2"','NUMKEYFRAMES="3"'), '<!DOCTYPE x>'+XAF):
            with self.subTest(text=text),self.assertRaises(ValueError):animation(self.read(text,'xaf'),bones)


if __name__=='__main__':unittest.main()

"""Regressions for the external-character-index tooling.

Two things here are worth a test because getting them wrong fails *silently*:

1. `ccip.cross()` replaces `model_metrics.onnx` with `(1 - cos) / 2`. If that
   equivalence ever stops holding (different CCIP variant, different export),
   every distance shifts and every threshold quietly stops meaning what it meant.
2. `build_characters.split_danbooru_tag()` drives the skin/base merge. A wrong
   split silently scatters one character across several rows.
"""

import sys
import sqlite3
import tempfile
import unittest
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

import build_characters  # noqa: E402
import ccip  # noqa: E402
import char_scan  # noqa: E402


class AnchorAliasTest(unittest.TestCase):
    """Indistinguishable anchors file claims under their owner; distinct characters never chain together."""

    def index(self, vectors, rows):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'source.db'
            with sqlite3.connect(path) as c:
                c.execute('CREATE TABLE characters(char_id,anchor_p90,copyright,wd_tag,base_tag,skin,post_count,gender_conf,anchor_p50,anchor_p75)')
                c.executemany('INSERT INTO characters VALUES (?,?,?,?,?,?,?,?,?,?)', [
                    (ident, .08, 'one', None, ident, skin, posts, gender, .03, .05) for ident, skin, posts, gender in rows])
            return char_scan.anchor_index(np.asarray(vectors, np.float32), [r[0] for r in rows], path)

    def test_mascot_and_skin_anchors_file_claims_under_the_character(self):
        # oz_(genshin_impact)'s reference images are Fischl; Puck's are Emilia (2026-09-25: 979 images lost their character).
        _, th, _, _, alias = self.index([[1, 0]] * 3 + [[0, 1]], [
            ('fischl', None, 3499, 1.), ('oz', None, 559, .6), ('fischl_summer', 'summer', 9000, 1.), ('bob', None, 10, 1.)])
        self.assertEqual(alias, ['fischl', 'fischl', 'fischl', 'bob'])
        np.testing.assert_allclose(th, .08)

    @staticmethod
    def arc(*angles):
        """Unit vectors on a circle; cross() distance is (1 - cos) / 2."""
        return [[np.cos(t), np.sin(t)] for t in angles]

    def test_low_scoring_hub_does_not_bridge_two_characters(self):
        # Single-linkage turned heixiu ~ luo_xiaohei ~ suzuran into one person (2026-09-25).
        vecs = self.arc(0, .16, .32)   # neighbours ~0.0064 apart, ends ~0.025
        d = ccip.cross(np.asarray(vecs, np.float32), np.asarray(vecs, np.float32), check=False)
        self.assertLessEqual(d[0, 1], char_scan.ALIAS_TH)
        self.assertGreater(d[0, 2], char_scan.ALIAS_TH)
        _, _, _, _, alias = self.index(vecs, [('suzuran', None, 3351, 1.), ('collab_mascot', None, 50, .5), ('other_girl', None, 900, 1.)])
        self.assertEqual(alias, ['suzuran', 'suzuran', 'other_girl'])

    def test_uphill_hops_reach_the_character(self):
        # mori_calliope_sleepwear only neighbours death_sensei, which neighbours mori_calliope.
        _, _, _, _, alias = self.index(self.arc(0, .16, .32), [('mori_calliope', None, 9000, 1.), ('death_sensei', None, 516, 1.), ('sleepwear', None, 351, 1.)])
        self.assertEqual(alias, ['mori_calliope'] * 3)


class SplitDanbooruTagTest(unittest.TestCase):
    def test_plain_name(self):
        self.assertEqual(build_characters.split_danbooru_tag("murosaki_miyo"),
                         ("murosaki_miyo", None, None))

    def test_name_and_copyright(self):
        self.assertEqual(build_characters.split_danbooru_tag("mika_(blue_archive)"),
                         ("mika", None, "blue_archive"))

    def test_skin_variant(self):
        self.assertEqual(
            build_characters.split_danbooru_tag("mika_(swimsuit)_(blue_archive)"),
            ("mika", "swimsuit", "blue_archive"))

    def test_single_qualifier_beside_a_plain_character_is_its_skin(self):
        rows = [('mori', 'mori_calliope', 'mori_calliope', None, 'hololive'),
                ('mori_sleep', 'mori_calliope_(sleepwear)', 'mori_calliope', None, 'sleepwear'),
                ('mika', 'mika_(blue_archive)', 'mika', None, 'blue_archive'),       # no plain mika: a copyright
                ('kasumi', 'kasumi_(doa)', 'kasumi', None, 'doa'),
                ('mori_swim', 'mori_calliope_(swimsuit)_(hololive)', 'mori_calliope', 'swimsuit', 'hololive')]
        self.assertEqual(build_characters.fold_plain_skins(rows), {'mori_sleep': ('sleepwear', 'hololive')})

    def test_multiple_subtitles(self):
        base, skin, cr = build_characters.split_danbooru_tag(
            "hoshino_(young)_(swimsuit)_(blue_archive)")
        self.assertEqual((base, cr), ("hoshino", "blue_archive"))
        self.assertEqual(skin, "young_swimsuit")


class CcipCrossTest(unittest.TestCase):
    """cross() must agree with the real model_metrics.onnx."""

    @classmethod
    def setUpClass(cls):
        feats = ROOT / "review" / "full-features.npy"
        if not feats.exists():
            raise unittest.SkipTest("review/full-features.npy not present")
        cls.F = np.load(feats)[:120]

    def test_matches_model_metrics(self):
        ref = ccip.differences(self.F)
        got = ccip.cross(self.F, self.F, check=False)
        self.assertLess(float(np.abs(ref - got).max()), 1e-4)

    def test_rectangular_block_matches(self):
        a, b = self.F[:40], self.F[40:]
        ref = ccip.differences(np.vstack([a, b]))[:len(a), len(a):]
        got = ccip.cross(a, b, check=False)
        self.assertLess(float(np.abs(ref - got).max()), 1e-4)

    def test_builtin_check_raises_on_tampered_input(self):
        """The self-check has to actually fire, otherwise it is decoration."""
        a = self.F[:60].copy()
        real_diff = ccip.differences

        def fake(x):
            return real_diff(x) + 0.5        # pretend the metric model disagrees

        ccip.differences = fake
        try:
            with self.assertRaises(AssertionError):
                ccip.cross(a, a, check=True)
        finally:
            ccip.differences = real_diff


if __name__ == "__main__":
    unittest.main()

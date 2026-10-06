import unittest
from unittest.mock import patch
from pathlib import Path

import numpy as np

from web.features import Features, pair_block_rows, PAIR_BLOCK_BYTES
from web.learn import _groups
from web.similar import grid_pairs, ccip_pairs, GRID_MATCH, CCIP_NEAR


def dense_groups(X, radius, minimum, limit=None):
    near = Features.distance(X, X) <= radius
    free, result = np.ones(len(X), bool), []
    for i in np.argsort(-near.sum(1)):
        if not free[i]: continue
        members = np.flatnonzero(near[i] & free)
        if len(members) < minimum: continue
        free[members] = False
        result.append(members)
        if limit and len(result) >= limit: break
    return result


class PairwiseTest(unittest.TestCase):
    def test_bitpacked_groups_equal_dense_across_block_and_byte_boundaries(self):
        rng = np.random.default_rng(10)
        for n in (0,1,7,9,100,517):
            X = rng.normal(size=(n,16)).astype(np.float32)
            X /= np.maximum(np.linalg.norm(X,axis=1,keepdims=True),1e-12)
            for minimum, limit in ((1,None),(3,None),(3,4)):
                with patch('web.learn.pair_block_rows',return_value=3):
                    actual = _groups(X,.3,minimum,limit)
                expected = dense_groups(X,.3,minimum,limit)
                self.assertEqual([x.tolist() for x in actual],[x.tolist() for x in expected])

    def test_similarity_candidates_equal_dense(self):
        rng = np.random.default_rng(11)
        X = rng.normal(size=(30,8)).astype(np.float32)
        X[1] = X[0]; X[3] = X[2]
        f = Features(Path('/unused'),X,[str(i) for i in range(30)])
        S = f.X @ f.X.T
        expected = {(str(i),str(j)) if str(i)<str(j) else (str(j),str(i))
                    for i,j in np.argwhere(S > 1-2*CCIP_NEAR) if i!=j}
        with patch('web.similar.pair_block_rows',return_value=3):
            self.assertEqual(ccip_pairs(f,f.uuids),expected)
        pixels = rng.integers(0,256,(8,256),dtype=np.uint8)
        pixels[1] = pixels[0]
        with patch('web.similar.pair_block_rows',return_value=3):
            self.assertEqual(grid_pairs([str(i) for i in range(8)],[1.]*8,[r.tobytes() for r in pixels]),{('0','1')})

    def test_matrix_budget(self):
        for n in (100,1000,48000,100000):
            self.assertLessEqual(pair_block_rows(n)*n*4,PAIR_BLOCK_BYTES)

    def test_crowded_ties_are_deterministic_across_blocks(self):
        pixels = np.random.default_rng(9).integers(0,256,256,dtype=np.uint8).tobytes()
        args = ([str(i) for i in range(30)], [1.]*30, [pixels]*30)
        expected = {tuple(sorted((u,v))) for u in args[0] for v in sorted(x for x in args[0] if x!=u)[:12]}
        for block in (3,30):
            with patch('web.similar.pair_block_rows',return_value=block):
                self.assertEqual(grid_pairs(*args),expected)

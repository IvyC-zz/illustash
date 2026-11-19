"""用极小的合成图像标注验证实验分母、角色去重和裁图对照。"""
import unittest

import numpy as np

from score import check_disjoint, measure, predict_views, shared_reference_ids


class CropScoreTest(unittest.TestCase):
    def test_counts_eligible_labels_and_paired_changes(self):
        rows = [
            dict(id=1, cohort='positive', solo=False, characters=['a', 'b', 'unknown'],
                 gender=['1girl', '1boy'], flags=[]),
            dict(id=2, cohort='single_control', solo=True, characters=['other'], gender=['1girl'], flags=[]),
        ]
        receipts = {1: dict(size=[2000, 2000], source='original'), 2: dict(size=[1000, 800], source='gel_webp_4m')}
        metrics = measure(rows, receipts, {1: {'a', 'b'}, 2: {'a'}}, {'a', 'b', 'c'}, {1: {'a', 'c'}, 2: set()})
        counts = metrics['groups']['all']
        self.assertEqual((counts['annotated_pairs'], counts['targets'], counts['true_pairs']), (4, 2, 2))
        self.assertEqual((counts['gained_pairs'], counts['lost_pairs']), (1, 0))
        self.assertEqual((counts['newly_false_images'], counts['cleared_false_images']), (1, 1))
        self.assertEqual(metrics['groups']['original/1_to_4mp']['targets'], 2)
        self.assertEqual(metrics['groups']['multiple_count/positive_multitag']['images'], 1)
        self.assertEqual(metrics['roles']['a']['wrong'], 1)

    def test_per_image_union_keeps_whole_and_deduplicates_regions(self):
        data = {1: dict(person=np.zeros((2, 4)), head=np.zeros((0, 4)))}
        lookup = [{'a'}, {'b'}, {'b'}]
        def select(_, kind, layer):
            return set().union(*lookup[layer])
        results = predict_views([dict(id=1)], data, select)
        self.assertEqual(results['whole+person_if_multiple'][1], {'a', 'b'})
        self.assertEqual(results['whole+head_if_multiple'][1], {'a'})
        self.assertEqual(results['head'][1], set())

    def test_pairs_use_the_same_five_single_subject_references(self):
        refs = [dict(id=i, solo=True, characters=['a']) for i in range(7)]
        refs[5]['characters'].append('different')
        counts = {r['id']: dict(person=[0], head=[0]) for r in refs}
        counts[6]['person'] = [0, 1]
        roles = {'a': list(range(7))}
        self.assertEqual(shared_reference_ids(roles, refs, counts, 'person'), {'a': list(range(5))})
        self.assertEqual(shared_reference_ids(roles, refs, counts, 'head'), {'a': [0, 1, 2, 3, 4, 6]})

    def test_post_family_leakage_is_rejected(self):
        common = dict(artist=None, md5=None, pixel_hash=None)
        with self.assertRaisesRegex(ValueError, '同组帖子'):
            check_disjoint([dict(common, id=1, parent_id=None)], [dict(common, id=2, parent_id=1)])

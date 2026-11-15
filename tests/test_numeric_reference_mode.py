"""独立参考自身的边界：授权范围、失败行为和非浮点业务契约。"""
import os
import io
import json
from pathlib import Path
import shutil
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import numpy as np
import PIL

from tests import numeric_reference_mode as mode
from tests import numpy_index_reference as index
from tests import numeric_reference_assertions as checks
from tests import numpy_reference_environment as environment


class NumericReferenceContractTest(unittest.TestCase):
    def fixture(self, name):
        cls, method = name.split('.')
        return type(cls, (), {'_testMethodName': method, 'id': lambda _: 'tests.' + name})()

    def test_default_stays_frozen_and_only_nine_methods_are_allowed(self):
        self.assertEqual(len(mode.METHODS), 9)
        with patch.dict(os.environ, clear=False):
            os.environ.pop(mode.ENV, None)
            for name in mode.METHODS:
                self.assertFalse(mode.enabled(self.fixture(name)))
            with self.assertRaisesRegex(AssertionError, '未批准'):
                mode.enabled(self.fixture('NativeIndexComputeTest.test_scan_exact_threshold_and_adjacent_float_inputs_match_old_csv'))

    def test_explicit_window_never_falls_back_or_skips_missing_capabilities(self):
        fixture = self.fixture(next(iter(mode.METHODS)))
        with patch.dict(os.environ, {mode.ENV: 'numpyy'}):
            with self.assertRaisesRegex(ValueError, '必须为'):
                mode.enabled(fixture)
        with patch.dict(os.environ, {mode.ENV: 'numpy'}), patch.object(mode.sys, 'platform', 'win32'):
            fixture._missing_numeric_candidate = 'candidate missing'
            with self.assertRaisesRegex(RuntimeError, 'candidate missing'):
                mode.check_candidate(fixture)
            sibling = self.fixture('NativeProvenanceVerifyTest.test_unapproved')
            sibling._missing_numeric_candidate = 'candidate missing'
            with self.assertRaises(unittest.SkipTest):
                mode.check_candidate(sibling)
            with patch.object(np, '__version__', 'unsupported'):
                with self.assertRaisesRegex(RuntimeError, '不能回退'):
                    mode.enabled(fixture)
            with (patch.object(np, '__version__', '2.5.3'), patch.object(PIL, '__version__', '12.3.0'),
                  patch.dict(os.environ, {'NPY_DISABLE_CPU_FEATURES': 'AVX2,FMA3,AVX512F,X86_V3,X86_V4'})):
                with self.assertRaisesRegex(RuntimeError, 'NPY_DISABLE_CPU_FEATURES'):
                    mode.enabled(fixture)
        with patch.dict(os.environ, {mode.ENV: 'numpy'}), patch.object(mode.sys, 'platform', 'linux'):
            with self.assertRaisesRegex(RuntimeError, '不能跳过'):
                mode.enabled(fixture)

    def test_registered_identity_adds_frozen_and_every_component_matters(self):
        fixture = self.fixture(next(iter(mode.METHODS)))
        with (patch.dict(os.environ, {mode.ENV: 'numpy'}), patch.object(mode.sys, 'platform', 'win32'),
              patch.object(np, '__version__', '2.5.3'), patch.object(PIL, '__version__', '12.3.0')):
            with patch.object(environment, 'environment', return_value=dict(environment.FROZEN_ENVIRONMENT)):
                self.assertEqual(mode.references(fixture), ('numpy', 'frozen'))
            for key in environment.FROZEN_ENVIRONMENT:
                value = dict(environment.FROZEN_ENVIRONMENT, **{key: 'unregistered'})
                with self.subTest(component=key), patch.object(environment, 'environment', return_value=value):
                    self.assertEqual(mode.references(fixture), ('numpy',))
            for feature in environment.FROZEN_ENVIRONMENT['numpy_dispatch']:
                value = dict(environment.FROZEN_ENVIRONMENT,
                             numpy_dispatch=environment.FROZEN_ENVIRONMENT['numpy_dispatch'] | {feature: False})
                with self.subTest(feature=feature), patch.object(environment, 'environment', return_value=value):
                    self.assertEqual(mode.references(fixture), ('numpy',))

    def test_failed_frozen_comparison_propagates_and_summary_keeps_it(self):
        from tests.windows_fixed_suite import reference_summary
        fixture = self.fixture(next(iter(mode.METHODS)))
        output = io.StringIO()
        with patch.object(mode.sys, '__stderr__', output):
            with mode.comparing(fixture, 'numpy'):
                pass
            with self.assertRaisesRegex(AssertionError, 'fixed mismatch'):
                with mode.comparing(fixture, 'frozen'):
                    raise AssertionError('fixed mismatch')
        records = [json.loads(line.removeprefix('NUMERIC_REFERENCE ')) for line in output.getvalue().splitlines()]
        self.assertEqual([r['status'] for r in records], ['passed', 'failed'])
        summary = '\n'.join(reference_summary(records))
        self.assertIn('| frozen | 0 | 1 |', summary)
        self.assertIn('| numpy | 1 | 0 |', summary)

    def test_missing_candidate_is_scoped_to_selected_methods_in_reused_setup(self):
        from tests import test_native_provenance_match as matching, test_native_provenance_verify as gallery
        selected = matching.NativeProvenanceMatchTest('test_stored_ratio_float32_boundary_and_flat_rejection_are_exact')
        other = matching.NativeProvenanceMatchTest('test_cached_signatures_thresholds_machine_only_and_repeat')
        inherited = gallery.NativeProvenanceVerifyTest(unittest.defaultTestLoader.getTestCaseNames(gallery.NativeProvenanceVerifyTest)[0])
        with patch.dict(os.environ, {mode.ENV: 'numpy'}):
            os.environ.pop('ILLUSTASH_PROVENANCE_COMPUTE_BIN', None)
            result = unittest.TestResult()
            unittest.TestSuite([selected, other, inherited]).run(result)
        self.assertEqual([test.id() for test, _ in result.errors], [selected.id()])
        self.assertEqual([test.id() for test, _ in result.skipped], [other.id(), inherited.id()])
        self.assertFalse(result.failures)

    def test_reference_summary_reads_actual_verbose_buffered_output(self):
        from tests.windows_fixed_suite import reference_record, reference_summary
        name = 'test_merge_percentile_core_and_partition_ties'
        for references in (('numpy',), ('numpy', 'frozen')):
            def compare(test):
                for reference in references:
                    with mode.comparing(test, reference):
                        pass
            fixture = type('NativeIndexComputeTest', (unittest.TestCase,), {name: compare})(name)
            output = io.StringIO()
            with patch.object(mode.sys, '__stderr__', output):
                result = unittest.TextTestRunner(stream=output, verbosity=2, buffer=True).run(unittest.TestSuite([fixture]))
            self.assertTrue(result.wasSuccessful())
            lines = output.getvalue().splitlines()
            self.assertGreater(lines[0].index('NUMERIC_REFERENCE '), 0)
            records = [record for line in lines if (record := reference_record(line)) is not None]
            self.assertEqual([record['reference'] for record in records], list(references))
            summary = '\n'.join(reference_summary(records))
            for reference in references:
                self.assertIn(f'| {reference} | 1 | 0 |', summary)

    def test_npy_rejects_dtype_shape_order_and_data_bit_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            values = np.array([[0, 2], [3, 4]], np.float32)
            np.save(root / 'expected.npy', values)
            signed_zero = values.copy(); signed_zero[0, 0] = -0.0
            for changed in (values.astype(np.float64), values.reshape(4), values + 1,
                            np.asfortranarray(values), signed_zero):
                np.save(root / 'actual.npy', changed)
                with self.assertRaises(AssertionError):
                    checks.assert_npy(self, root / 'actual.npy', root / 'expected.npy')
            np.save(root / 'fortran.npy', np.asfortranarray(values))
            with self.assertRaises(AssertionError):
                checks.assert_npy(self, root / 'fortran.npy', root / 'fortran.npy', c_order=True)
            # 两个维度中有 1 时，数组 flags 不能分辨头部声明；消费端仍拒绝 Fortran。
            with (root / 'fortran.npy').open('wb') as output:
                np.lib.format.write_array_header_1_0(output, dict(descr='<f4', fortran_order=True, shape=(1, 4)))
                output.write(values.tobytes())
            with self.assertRaises(AssertionError):
                checks.assert_npy(self, root / 'fortran.npy', root / 'fortran.npy', c_order=True)
            with (root / 'actual.npy').open('wb') as output:
                np.lib.format.write_array_header_2_0(output, dict(descr='<f4', fortran_order=False, shape=values.shape))
                output.write(values.tobytes())
            checks.assert_npy(self, root / 'actual.npy', root / 'expected.npy', c_order=True)

    def test_index_persistent_ids_metadata_rejection_and_csv_bytes_are_checked(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); expected = root / 'expected'; expected.mkdir()
            for name in ('char-anchors.npy', 'char-index/new.npy', 'char-patch/raw/new.npy', 'char-patch/raw/twin.npy'):
                path = expected / name; path.parent.mkdir(exist_ok=True, parents=True)
                np.save(path, np.ones((2, 3), np.float32))
            (expected / 'char-patch/rejected').mkdir()
            for name in ('char-index/new.json', 'char-patch/rejected/twin.json'):
                (expected / name).write_text('{"posts":[0,1],"refs_kept":2}')
            (expected / 'char-anchors.txt').write_text('first\nsecond')
            (expected / 'char-scan.csv').write_bytes(b'uuid,char_id\r\na,first\r\n')
            for index, (name, content) in enumerate((
                ('char-anchors.txt', b'second\nfirst'),
                ('char-index/new.json', b'{"posts":[0,1],"refs_kept":3}'),
                ('char-patch/rejected/twin.json', b'{"posts":[1,0],"refs_kept":2}'),
                ('char-scan.csv', b'uuid,char_id\na,first\n'),
            )):
                actual = root / str(index); shutil.copytree(expected, actual)
                (actual / name).write_bytes(content)
                with self.subTest(name=name), self.assertRaises(AssertionError):
                    checks.assert_index_files(self, actual, expected, cached=True)

    def test_cache_includes_original_due_cursor_and_preserves_untouched_tables(self):
        from tests import provenance_fixture
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with provenance_fixture.connect(type('Library', (), {'path': root})()) as db:
                db.execute("INSERT INTO artists(id) VALUES ('A')")
                db.execute("INSERT INTO ref_sigs(site,post_id,ratio,grid) VALUES ('example','1',1,X'00')")
            before = checks.cache_state(root)
            started = checks.utc_second()
            with sqlite3.connect(root / 'provenance.sqlite') as db:
                db.execute("INSERT INTO image_sources(asset,site,post_id,url,method) VALUES ('a','example','1','','gallery_match')")
                db.execute("INSERT INTO lookups(target,backend,last_at) VALUES ('a','ref_compare',datetime('now'))")
            finished = checks.utc_second()
            expected = checks.cache_state(root)
            checks.assert_cache_preserved(self, before, expected, started, finished)
            for statement in (
                "UPDATE image_sources SET original='changed'",
                "UPDATE lookups SET next_due_at='2099-01-01 00:00:00'",
                "UPDATE lookups SET cursor='changed'",
                "DELETE FROM ref_sigs",
                "UPDATE artists SET merged_into='A'",
            ):
                with sqlite3.connect(root / 'provenance.sqlite') as db:
                    db.execute('BEGIN')
                    db.execute(statement)
                    # 未提交的值在同一连接导出，再放进隔离副本，避免污染下一子例。
                    sql = '\n'.join(db.iterdump())
                    db.rollback()
                changed = root / 'changed'; changed.mkdir(exist_ok=True)
                (changed / 'provenance.sqlite').unlink(missing_ok=True)
                with sqlite3.connect(changed / 'provenance.sqlite') as db:
                    db.executescript(sql)
                    db.execute('PRAGMA user_version=5')
                state = checks.cache_state(changed)
                self.assertNotEqual(checks.comparable_cache(state), checks.comparable_cache(expected), statement)
                if 'ref_sigs' in statement or 'artists' in statement:
                    with self.assertRaises(AssertionError):
                        checks.assert_cache_preserved(self, before, state, started, finished)
            with sqlite3.connect(root / 'provenance.sqlite') as db:
                db.execute("UPDATE image_sources SET fetched_at='1999-01-01 00:00:00'")
            with self.assertRaises(AssertionError):
                checks.assert_cache_preserved(self, before, checks.cache_state(root), started, finished)

    def test_scalar_sort_keeps_fixed_partition_order_and_rejects_invalid_input(self):
        values = np.ones(10, np.float32)
        self.assertEqual(index.scalar_order(values, partition=True).tolist(), [6, 4, 8, 7, 0, 5, 3, 1, 2])
        for values in (np.array([np.nan], np.float32), np.ones(2, np.float64), np.ones((2, 2), np.float32)):
            with self.assertRaises(ValueError):
                index.scalar_order(values)

    def test_alias_hard_boundary_metadata_priority_and_uphill_walk(self):
        # 用手写矩阵独立约束 <= .01，避免用参考算法自己产生阈值期望。
        edge = np.float32(.01)
        above = np.nextafter(edge, np.float32(np.inf))
        distances = np.array([[0, edge, above], [edge, 0, edge], [above, edge, 0]], np.float32)
        ids = ['a', 'b', 'c']
        def meta(skin, posts, gender):
            return (None, None, None, None, skin, posts, gender, None, None)
        quality = {'a': meta(None, 10000, .89), 'b': meta('skin', 100, .9), 'c': meta(None, 1, .9)}
        with patch.object(index, 'cross', return_value=distances):
            self.assertEqual(index.anchor_alias(np.zeros((3, 768), np.float32), ids, quality), ['c', 'c', 'c'])
        # 将 b→c 移至阈值之外，a 的高热度不能越过人物质量和本体优先级。
        distances[1, 2] = distances[2, 1] = above
        with patch.object(index, 'cross', return_value=distances):
            self.assertEqual(index.anchor_alias(np.zeros((3, 768), np.float32), ids, quality), ['b', 'b', 'c'])


if __name__ == '__main__':
    unittest.main()

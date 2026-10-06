"""#77：坏 worker 响应/恢复文件包含文件与字段；合成宿主仍可浏览和人工标记。"""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

from contextlib import contextmanager
import json
import sqlite3
import os
import unittest
from pathlib import Path
from tests import test_native_index as index_fixture
from tests import test_native_sync as sync_fixture
from tests import test_native_provenance as provenance_fixture
from tests import test_native_research as research_fixture


class NativeWorkerErrorsTest(unittest.TestCase):
    @contextmanager
    def fixture(self, kind):
        kind.setUpClass()
        case = kind(methodName=('test_native_library_new_worker_per_run_failure_retry_and_identity'
                                if kind is sync_fixture.NativeSyncTest else 'runTest'))
        try:
            case.setUp()
            yield case
        finally:
            case.doCleanups()

    def corrupt(self, script, marker, field, wrong):
        text = script.read_text()
        self.assertIn(marker, text)
        injection = f"v[{field!r}]=False" if wrong else f"v.pop({field!r})"
        script.write_text(text.replace(marker, injection + '\n' + marker))

    def field_error(self, message, field, file='response.json'):
        self.assertIn(field, message)
        self.assertIn(file, message)

    def usable(self, case):
        response = case.request('/api/assets')
        self.assertEqual(response['status'], 200, response)
        with sqlite3.connect(case.lib.db) as db:
            asset = db.execute('SELECT uuid FROM assets ORDER BY uuid LIMIT 1').fetchone()[0]
        response = case.request('/api/tag', {'ids': [asset], 'tag': 'synthetic-worker-error'})
        self.assertEqual(response['status'], 200, response)
        with sqlite3.connect(case.lib.db) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM asset_tags WHERE uuid=? AND tag='synthetic-worker-error'", (asset,)).fetchone()[0], 1)

    def test_index_response_missing_and_wrong_field_keeps_host_usable(self):
        for wrong in (False, True):
            with self.subTest(wrong=wrong), self.fixture(index_fixture.NativeIndexTest) as case:
                script = worker_source(case.env['ILLUSTASH_INDEX_WORKER'])
                self.corrupt(script, 'Path(sys.argv[2]).write_text(json.dumps(v))', 'labels', wrong)
                case.check()
                case.index({'action': 'apply', 'tags': ['carol_example']})
                state = case.finish()['state']
                self.assertEqual(state['status'], 'error', state)
                self.field_error(state['message'], 'labels')
                self.usable(case)

    def test_sync_response_missing_and_wrong_field_keeps_host_usable(self):
        for wrong in (False, True):
            with self.subTest(wrong=wrong), self.fixture(sync_fixture.NativeSyncTest) as case:
                case.worker()
                script = worker_source(case.env['ILLUSTASH_RECOGNITION_WORKER'])
                marker = "Path(sys.argv[2]).write_text(json.dumps(dict(identity=r['identity'],packages=packages,failed=[])))"
                text = script.read_text(); self.assertIn(marker, text)
                script.write_text(text.replace(marker, "v=dict(identity=r['identity'],packages=packages,failed=[])\nPath(sys.argv[2]).write_text(json.dumps(v))"))
                self.corrupt(script, 'Path(sys.argv[2]).write_text(json.dumps(v))', 'packages', wrong)
                state = case.sync()['state']
                self.assertEqual(state['status'], 'error', state)
                self.field_error(state['message'], 'packages')
                self.usable(case)

    def test_provenance_response_missing_and_wrong_field_keeps_host_usable(self):
        for wrong in (False, True):
            with self.subTest(wrong=wrong), self.fixture(provenance_fixture.NativeProvenanceTest) as case:
                marker = "Path(sys.argv[2]).write_text(json.dumps({'format':r['format'],'identity':r['identity'],'step':step,'result':result}))"
                text = case.worker.read_text(); self.assertIn(marker, text)
                case.worker.write_text(text.replace(marker, "v={'format':r['format'],'identity':r['identity'],'step':step,'result':result}\nPath(sys.argv[2]).write_text(json.dumps(v))"))
                self.corrupt(case.worker, 'Path(sys.argv[2]).write_text(json.dumps(v))', 'result', wrong)
                case.post({'action': 'artist', 'artist': 'synthetic'})
                state = case.finish('failed')['state']
                self.field_error(state['reason'], 'result')
                self.usable(case)

    def test_research_response_missing_and_wrong_field_keeps_host_usable(self):
        for wrong in (False, True):
            with self.subTest(wrong=wrong), self.fixture(research_fixture.NativeResearchTest) as case:
                self.corrupt(case.worker, 'Path(sys.argv[2]).write_text(json.dumps(v,allow_nan=False))', 'version', wrong)
                response = case.request('/api/similar', {})
                self.assertEqual(response['status'], 200, response)
                case.finished('error')
                self.field_error(case.state()['message'], 'version')
                self.usable(case)

    def test_index_journal_missing_and_wrong_field_keeps_host_usable(self):
        for wrong in (False, True):
            with self.subTest(wrong=wrong), self.fixture(index_fixture.NativeIndexTest) as case:
                case.mode.write_text('fail')
                case.check()
                case.index({'action': 'apply', 'tags': ['carol_example']}); case.finish()
                file = case.lib.path / 'index-job.json'
                journal = json.loads(file.read_text())
                if wrong: journal['job_id'] = False
                else: del journal['job_id']
                file.write_text(json.dumps(journal))
                response = case.request('/api/index', {'action': 'apply', 'tags': ['carol_example']})
                self.assertEqual(response['status'], 400, response)
                self.field_error(json.loads(response['body'])['error'], 'job_id', 'index-job.json')
                self.usable(case)

    def test_invalid_persisted_provenance_status_has_context_and_keeps_host_usable(self):
        with self.fixture(provenance_fixture.NativeProvenanceTest) as case:
            case.post({'action': 'artist', 'artist': 'synthetic'}); case.finish('done')
            case.stop(case.process)
            with sqlite3.connect(case.lib.path / 'provenance.sqlite') as db:
                db.execute("UPDATE runs SET status='unknown-status'")
            case.restart()
            state = case.info()['state']
            self.assertEqual(state['status'], 'failed', state)
            self.field_error(state['reason'], 'status', 'provenance.sqlite')
            self.usable(case)

    def test_index_database_receipt_keeps_existing_json_bytes(self):
        with self.fixture(index_fixture.NativeIndexTest) as case:
            state = case.apply()['state']
            self.assertEqual(state['status'], 'done', state)
            with sqlite3.connect(case.lib.source) as db:
                receipt = db.execute('SELECT result FROM native_index_runs').fetchone()[0]
            value = json.loads(receipt)
            self.assertNotIn('result', value)
            self.assertEqual(receipt, json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')))

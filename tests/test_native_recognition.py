"""Rust recognition imports: owned synthetic resources/packages, no models or accounts.

The former Python importer's result on the same package was recorded through
tests/fixed_expectations.py (#9).
"""
import concurrent.futures
import hashlib
import json
import shutil
import sqlite3
import subprocess
import time
import os
import unittest
from pathlib import Path
from unittest.mock import patch
import numpy as np
from tests import test_native_lifecycle as lifecycle
from tests.fixed_expectations import unclocked
from tests.test_native_human_labels import snapshot, freeze_seed_defaults

# The recognition package files and the source tables they land in (input contract of the former importer).
FILES = ('labels.jsonl', 'wd.npy', 'ccip.npy')
SCHEMA = """CREATE TABLE IF NOT EXISTS recognition_imports(
 digest TEXT PRIMARY KEY, manifest TEXT NOT NULL, assets TEXT NOT NULL, status TEXT NOT NULL,
 error TEXT NOT NULL DEFAULT '', imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
 CREATE TABLE IF NOT EXISTS recognition_assets(
 asset TEXT PRIMARY KEY, digest TEXT NOT NULL, package TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model));"""


class NativeRecognitionTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    package_library = lifecycle.NativeLifecycleTest.package
    package = lifecycle.NativeLifecycleTest.package
    stop = lifecycle.NativeLifecycleTest.stop
    start = lifecycle.NativeLifecycleTest.start
    request = lifecycle.NativeLifecycleTest.request

    def setUp(self):
        lifecycle.NativeLifecycleTest.setUp(self)
        # Restart with synthetic fingerprints; never read the installed model files.
        self.stop(self.process)
        self.resources = self.root / 'resources'
        (self.resources / 'models' / 'ccip').mkdir(parents=True)
        (self.resources / 'models' / 'wd_swinv2_v3_emb.onnx').write_bytes(b'synthetic WD')
        (self.resources / 'models' / 'ccip' / 'model_feat.onnx').write_bytes(b'synthetic CCIP')
        (self.resources / 'tags').mkdir()
        (self.resources / 'tags' / 'tags_v3.csv').write_text('name,category\ngeneral,9\nblue_hair,0\nalice,4\n')
        self.specs = {
            'wd': dict(version='swinv2_448', dimension=1024, sha256=hashlib.sha256(b'synthetic WD').hexdigest(), tags_sha256=hashlib.sha256((self.resources / 'tags' / 'tags_v3.csv').read_bytes()).hexdigest()),
            'ccip': dict(version='ccip-model_feat', dimension=768, sha256=hashlib.sha256(b'synthetic CCIP').hexdigest())}
        with sqlite3.connect(self.lib.source) as c:
            for col in ('source TEXT DEFAULT "legacy"', 'model_ver TEXT', 'run_id TEXT'):
                c.execute('ALTER TABLE labels ADD COLUMN ' + col)
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def incoming(self, ids=('a', 'b'), name='result'):
        p = self.root / name
        p.mkdir(exist_ok=True)
        labels = [dict(asset=u, dim='rating', value='general', confidence=.9) for u in ids]
        labels += [dict(asset=u, dim='general', value='blue_hair', confidence=.7) for u in ids]
        (p / 'labels.jsonl').write_text('\n'.join(json.dumps(x) for x in labels), newline='\n')
        for model in ('wd', 'ccip'):
            np.save(p / (model + '.npy'), np.ones((len(ids), self.specs[model]['dimension']), dtype='<f4'))
        self.manifest(p, ids)
        return p

    def manifest(self, p, ids=('a', 'b')):
        m = dict(format='illustash-recognition/1', library_id=self.lib.config['id'], assets=list(ids), models=self.specs,
                 files={f:dict(bytes=(p/f).stat().st_size,sha256=hashlib.sha256((p/f).read_bytes()).hexdigest()) for f in FILES})
        (p / 'manifest.json').write_text(json.dumps(m), newline='\n')
        return m

    def cli(self, *args):
        return subprocess.run([str(self.binary), 'recognition-import', '--library', str(self.lib.path), '--root', str(self.resources), *args], env=self.env, capture_output=True, timeout=20)

    def import_result(self, p):
        r = self.request('/api/recognition/import', {'path': str(p)})
        self.assertEqual(r['status'], 200, r['body'])
        return json.loads(r['body'])

    def test_valid_duplicate_fingerprint_source_reference_and_human_history(self):
        p=self.incoming()
        # Seed a manual verdict and preserve it through native import and resume.
        self.assertEqual(self.request('/api/tag', {'ids':['a'], 'tag':'合成人工'})['status'],200)
        before=snapshot(self.lib.db)['actions']
        with sqlite3.connect(self.lib.source) as c: c.executescript(SCHEMA)
        freeze_seed_defaults(self.lib.source)
        freeze_seed_defaults(self.lib.db)
        expected=self.rec.value('old import')
        actual=self.import_result(p)
        source_now=dict(snapshot(self.lib.source))
        if os.name == 'nt':
            # The package digest differs on Windows from the macOS recording, and with it the rows
            # keyed by it (run ids, import records); everything else must match.
            actual,expected['result']=dict(actual,digest=None),dict(expected['result'],digest=None)
            for table in ('labels','recognition_assets','recognition_imports'):
                source_now.pop(table,None);expected['source'].pop(table,None)
        self.assertEqual(actual,expected['result'])
        self.assertEqual(source_now,expected['source'])
        # Random identity IDs/timestamps are checked by existing core golden tests; these tags/results are deterministic.
        for table in ('assets','assertions','asset_tags','memberships','general_hits','asset_rating','actions'):
            self.assertEqual(snapshot(self.lib.db)[table],expected['db'][table],table)
        self.assertEqual(snapshot(self.lib.db)['actions'],before)
        source=snapshot(self.lib.source); app=snapshot(self.lib.db)
        self.assertTrue(self.import_result(p)['duplicate'])
        self.assertEqual(snapshot(self.lib.source),source)
        self.assertEqual(snapshot(self.lib.db),app)
        # Different manifest with exactly the same asset results also deduplicates.
        m=json.loads((p/'manifest.json').read_text());m['producer_note']='合成';(p/'manifest.json').write_text(json.dumps(m),newline='\n')
        self.assertFalse(self.import_result(p)['duplicate'])
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0],2)
        self.assertFalse(self.sentinel.exists())

    def test_invalid_inputs_never_write(self):
        for bad in ('checksum','library','version','models','shape','nan','inf','zero','fortran','dtype','missing','case','repeat_id','repeat_label','confidence','bool','rating','symlink','header_expression'):
            with self.subTest(bad=bad):
                p=self.incoming();m=json.loads((p/'manifest.json').read_text())
                if bad=='checksum': (p/'wd.npy').write_bytes(b'bad')
                elif bad=='library': m['library_id']='wrong'
                elif bad=='version': m['format']='illustash-recognition/2'
                elif bad=='models': m['models']['wd']['sha256']='a'*64
                elif bad in ('shape','nan','inf','zero','fortran','dtype'):
                    x=np.ones((2,7 if bad=='shape' else 768),dtype='<f8' if bad=='dtype' else '<f4')
                    if bad in ('nan','inf','zero'): x[0,:]=dict(nan=np.nan,inf=np.inf,zero=0)[bad]
                    if bad=='fortran':x=np.asfortranarray(x)
                    np.save(p/'ccip.npy',x);m=self.manifest(p)
                elif bad in ('missing','case'): p=self.incoming(('missing' if bad=='missing' else 'A',));m=json.loads((p/'manifest.json').read_text())
                elif bad=='repeat_id':m['assets']=['a','a']
                elif bad in ('repeat_label','confidence','bool','rating'):
                    lines=(p/'labels.jsonl').read_text().splitlines()
                    if bad=='repeat_label': lines.append(lines[0])
                    else:
                        row=json.loads(lines[0]);row['confidence']= True if bad=='bool' else 1.1
                        if bad=='rating':row['value']='bad'
                        lines[0]=json.dumps(row)
                    (p/'labels.jsonl').write_text('\n'.join(lines));m=self.manifest(p)
                elif bad=='symlink':
                    target=self.root/'saved.npy';shutil.copyfile(p/'wd.npy',target);(p/'wd.npy').unlink();(p/'wd.npy').symlink_to(target)
                elif bad=='header_expression':
                    raw=(p/'wd.npy').read_bytes(); raw=raw.replace(b"'fortran_order': False",b"'fortran_order': True ");(p/'wd.npy').write_bytes(raw);m=self.manifest(p)
                (p/'manifest.json').write_text(json.dumps(m))
                source=snapshot(self.lib.source); app=snapshot(self.lib.db)
                r=self.request('/api/recognition/import',{'path':str(p)})
                self.assertEqual(r['status'],400)
                self.assertEqual(snapshot(self.lib.source),source)
                self.assertEqual(snapshot(self.lib.db),app)
                if (p/'wd.npy').is_symlink(): (p/'wd.npy').unlink()

    def test_source_failure_rollback_conflicting_result_and_cli(self):
        p=self.incoming()
        with sqlite3.connect(self.lib.source) as c:
            c.execute("CREATE TRIGGER fail BEFORE INSERT ON labels WHEN NEW.uuid='b' AND NEW.source='wd_v3' BEGIN SELECT RAISE(ABORT,'synthetic source failure'); END")
        before=snapshot(self.lib.source)
        self.assertEqual(self.request('/api/recognition/import',{'path':str(p)})['status'],500)
        self.assertEqual(snapshot(self.lib.source),before)
        with sqlite3.connect(self.lib.source) as c:c.execute('DROP TRIGGER fail')
        result=self.cli('--package',str(p));self.assertEqual(result.returncode,0,result.stderr)
        np.save(p/'wd.npy',np.full((2,1024),2,dtype='<f4'));self.manifest(p)
        before=snapshot(self.lib.source)
        self.assertEqual(self.request('/api/recognition/import',{'path':str(p)})['status'],400)
        self.assertEqual(snapshot(self.lib.source),before)
        self.assertEqual(json.loads(self.cli('--model-specs').stdout),self.specs)

    def test_application_failure_resume_restart_and_no_duplicate_formal_results(self):
        p=self.incoming()
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic app failure'); END")
        r=self.import_result(p);self.assertEqual(r['status'],'pending')
        self.assertEqual(self.cli('--resume').returncode,2)
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0],2)
            self.assertEqual(c.execute('SELECT status FROM recognition_imports').fetchone()[0],'pending')
        with sqlite3.connect(self.lib.db) as c:c.execute('DROP TRIGGER fail')
        self.stop(self.process);self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.request('/api/recognition/import',{'resume':True})['body'],b'{"status": "done"}')
        before=snapshot(self.lib.db)
        self.assertEqual(self.cli('--resume').returncode,0)
        self.assertEqual(snapshot(self.lib.db),before)

    def test_ack_failure_resume_does_not_repeat_projection(self):
        with sqlite3.connect(self.lib.db) as c:c.execute("CREATE TRIGGER fail BEFORE UPDATE ON backend_effects WHEN OLD.kind='imported' BEGIN SELECT RAISE(ABORT,'synthetic ack failure'); END")
        self.assertEqual(self.import_result(self.incoming())['status'],'pending')
        before=snapshot(self.lib.db)
        with sqlite3.connect(self.lib.db) as c:c.execute('DROP TRIGGER fail')
        self.assertEqual(json.loads(self.request('/api/recognition/import',{'resume':True})['body']),{'status':'done'})
        after=snapshot(self.lib.db)
        for table in ('term_evidence','term_evidence_sets','term_batches','term_projection','term_verdicts','actions','memberships'):
            self.assertEqual(before[table],after[table],table)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM backend_effects WHERE kind='imported'").fetchone()[0],1)

    def test_concurrent_duplicate_and_old_session_same_id_package(self):
        p=self.incoming()
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            out=list(pool.map(lambda _:self.import_result(p),range(4)))
        self.assertEqual(sum(not r['duplicate'] for r in out),1)
        old_token=self.token
        other=self.root/'other.pilib';shutil.copytree(self.lib.path,other)
        self.assertEqual(self.request('/api/libraries/open',{'path':str(other)})['status'],200)
        before=snapshot(other/'source.sqlite')
        self.assertEqual(self.request('/api/recognition/import',{'path':str(p)},token=old_token)['status'],403)
        self.assertEqual(snapshot(other/'source.sqlite'),before)

    def test_source_done_confirmation_failure_retries_only_confirmation(self):
        with sqlite3.connect(self.lib.source) as c:
            c.executescript(SCHEMA)
            c.execute("CREATE TRIGGER fail BEFORE UPDATE ON recognition_imports WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'synthetic confirmation failure'); END")
        self.assertEqual(self.request('/api/recognition/import',{'path':str(self.incoming())})['status'],500)
        before=snapshot(self.lib.db)
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT status FROM recognition_imports').fetchone()[0],'pending')
            c.execute('DROP TRIGGER fail')
        self.assertEqual(self.cli('--resume').returncode,0)
        self.assertEqual(snapshot(self.lib.db),before)

    def test_frozen_package_refuses_import_and_resume_without_writes(self):
        self.stop(self.process)
        cfg=json.loads((self.lib.path/'library.json').read_text());cfg['frozen']=True
        (self.lib.path/'library.json').write_text(json.dumps(cfg))
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        before=snapshot(self.lib.source)
        for body in ({'path':str(self.incoming())},{'resume':True}):
            self.assertEqual(self.request('/api/recognition/import',body)['status'],409)
        self.assertEqual(snapshot(self.lib.source),before)

    @unittest.skipUnless(hasattr(os, 'mkfifo'), 'a FIFO holds the import open; POSIX only')
    def test_import_runs_outside_the_session_lock_and_a_switch_cancels_it(self):
        # #26: the WD model file is a FIFO, so the import stays in its model fingerprint step until it is fed.
        p = self.incoming()
        model = self.resources / 'models' / 'wd_swinv2_v3_emb.onnx'
        model.unlink()
        os.mkfifo(model)
        before = snapshot(self.lib.source)
        other = self.root / 'other.pilib'
        shutil.copytree(self.lib.path, other)
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(self.request, '/api/recognition/import', {'path': str(p)})
            deadline = time.monotonic() + 10
            while True:  # opening the write end succeeds once the import has the FIFO open for reading
                try:
                    writer = os.open(model, os.O_WRONLY | os.O_NONBLOCK)
                    break
                except OSError:
                    self.assertLess(time.monotonic(), deadline, 'import never reached the model files')
                    time.sleep(0.01)
            self.addCleanup(lambda: os.close(writer) if writer is not None else None)
            started = time.monotonic()
            self.assertEqual(self.request('/api/bootstrap')['status'], 200)
            refused = self.request('/api/sync', {})
            self.assertEqual(refused['status'], 400, refused)
            self.assertIn('识别结果正在导入', json.loads(refused['body'])['error'])
            opened = self.request('/api/libraries/open', {'path': str(other)})
            self.assertEqual(opened['status'], 200, opened)
            self.assertLess(time.monotonic() - started, 5)
            self.assertFalse(pending.done())
            os.set_blocking(writer, True)
            os.write(writer, b'synthetic WD')
            os.close(writer)
            writer = None
            response = pending.result(timeout=20)
        self.assertEqual(response['status'], 409, response)
        self.assertIn('导入已取消', json.loads(response['body'])['error'])
        self.assertEqual(snapshot(self.lib.source), before)

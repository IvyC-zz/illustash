"""#29 synthetic historical source/app migrations; no real library or copied real data.

The historical packages were built, and their expected upgrades computed, by the former Python
application; both are recorded through tests/fixed_expectations.py (#9). Native keys are reproducible (store::new_key test counter).
"""
from contextlib import closing
import json
import datetime
import time
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from tests.fixed_expectations import Library, Expectations, unclocked
from tests.test_native_library_upgrade import database
from tests.test_read_http import SPAWN, free_port, wait_until
from tools.verify_read_http import fetch
from tests.synthetic_source import source_fixture as fixture


class LegacyUpgradeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary=Path(os.environ['ILLUSTASH_TERMS_BIN']).resolve()
        cls.resources=Path(os.environ.get('ILLUSTASH_NATIVE_ROOT') or cls.binary.parents[4]).resolve()

    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(prefix='illustash-issue29-'); self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve()
        self.env={**os.environ,'HOME':str(self.root/'home'),'USERPROFILE':str(self.root/'home'),'ILLUSTASH_APP_SUPPORT':str(self.root/'support'),'ILLUSTASH_SHARED':str(self.root/'shared'),
                  'ILLUSTASH_TEST_KEY_COUNTER':str(self.root/'key-counter')}
        self.rec=Expectations(self);self.addCleanup(self.rec.finish,self)
        self.rec.alias(self.root,'@ROOT@')

    def package(self,version):
        """A historical application of this version, as the former Python Store wrote it (recorded)."""
        path=self.root/f'v{version}.pilib'
        return self.rec.package(f'v{version}',path)


    def baseline(self,lib,after=None):
        """The former Python startup upgrade of a copy of ``lib`` (recorded), then ``after(store, expected)``."""
        path=self.root/(lib.path.stem+'-python.pilib')
        return self.rec.package(path.name,path)

    def run_native(self,lib,success=True):
        port=free_port();log=tempfile.TemporaryFile();self.addCleanup(log.close)
        args=[str(self.binary),'http','--no-auto-sync','--no-names','--upgrade','--library',str(lib.path),'--port',str(port),'--root',str(self.resources)]
        p=subprocess.Popen(args,env={**self.env,'PATH':'/nonexistent'},stdout=log,stderr=log, **SPAWN)
        def stop():
            if p.poll() is None:
                p.terminate()
                try:p.wait(5)
                except subprocess.TimeoutExpired:p.kill();p.wait()
        self.addCleanup(stop)
        if success:
            try:wait_until(lambda:fetch(f'http://127.0.0.1:{port}',{'url':'/api/bootstrap'},f'http://127.0.0.1:{port}')['status']==200,timeout=8)
            except AssertionError:
                log.seek(0);self.fail(log.read().decode(errors='replace'))
            stop()
        else:
            p.wait(10);self.assertNotEqual(p.returncode,0)
        log.seek(0);return log.read().decode(errors='replace')

    def equivalent(self, actual, expected, before, recorded=not False):
        """``recorded``: ``expected`` comes from a recording, made at another time."""
        left,right=database(actual),database(expected)
        self.assertEqual(left[0],right[0])
        # Independent producers have different clocks. Align only new rows' declared
        # server-generated timestamps; pre-existing history and every other cell stay exact.
        clocks={'links':'added_at','term_assets':'added_at','terms':'created_at','term_links':'added_at',
                'term_relations':'created_at','term_batches':'created_at','term_verdicts':'created_at',
                'term_evidence':'created_at','term_packages':'imported_at','term_evidence_sets':'created_at'}
        with closing(sqlite3.connect(actual)) as c:
            old_meta={r[1][1] for r in before[1].get('meta',[])}
            # A recorded expectation's clocks are from when it was recorded: only the live side is checked against now.
            live=lambda data:data is left[1] or not recorded
            for data in (left[1],right[1]):
                for i,row in enumerate(data['meta']):
                    if row[1][1] in ('indexed','general_indexed','rating_indexed') and row[1][1] not in old_meta:
                        value=row[2][1]
                        stamp=datetime.datetime.strptime(value,'%Y-%m-%d %H:%M:%S').timestamp()
                        if live(data):self.assertLess(abs(stamp-time.time()),60)
                        data['meta'][i]=(row[0],row[1],('str','<new-index-clock>'))
            for table,column in clocks.items():
                if table not in left[1]:continue
                cols=[r[1] for r in c.execute('PRAGMA table_info('+table+')')]
                pos=cols.index(column)+1  # these tables all have rowid
                old={row[0] for row in before[1].get(table,[])}
                for data in (left[1],right[1]):
                    aligned=[]
                    for row in data[table]:
                        row=list(row)
                        if row[0] not in old:
                            value=row[pos][1]
                            stamp=datetime.datetime.strptime(value,'%Y-%m-%d %H:%M:%S').replace(tzinfo=datetime.timezone.utc).timestamp()
                            if live(data):self.assertLess(abs(stamp-time.time()),60)
                            row[pos]=('str','<new-server-clock>')
                            if table=='term_assets':
                                pos_removed=cols.index('removed_at')+1
                                value=row[pos_removed][1]
                                if value is not None:
                                    stamp=datetime.datetime.strptime(value,'%Y-%m-%d %H:%M:%S').replace(tzinfo=datetime.timezone.utc).timestamp()
                                    if live(data):self.assertLess(abs(stamp-time.time()),60)
                                    row[pos_removed]=('str','<new-server-clock>')
                            if table=='term_packages':
                                pos_manifest=cols.index('manifest')+1
                                payload=json.loads(row[pos_manifest][1])
                                stamp=datetime.datetime.strptime(payload['created_at'],'%Y-%m-%dT%H:%M:%SZ').replace(tzinfo=datetime.timezone.utc).timestamp()
                                if live(data):self.assertLess(abs(stamp-time.time()),60)
                                payload['created_at']='<new-package-clock>'
                                row[pos_manifest]=('str',json.dumps(payload,ensure_ascii=False,sort_keys=True,separators=(',',':')))
                        aligned.append(tuple(row))
                    data[table]=aligned
        if recorded:
            # Against a recording, other clock-like cells written during either upgrade (history,
            # derived stamps) are from different runs: compared as <clock>; recording keeps them exact.
            for data in (left[1],right[1]):
                for table,rows in data.items():
                    data[table]=sorted([tuple((k,unclocked(v)) for k,v in row) for row in rows],key=repr)
        self.assertEqual(left[1],right[1])

    def test_equivalent_only_aligns_recent_new_asset_removal_clocks(self):
        actual=self.root/'clock-actual.sqlite';expected=self.root/'clock-expected.sqlite'
        with closing(sqlite3.connect(actual)) as c:
            c.executescript("""
            CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE term_assets(id INTEGER PRIMARY KEY,namespace TEXT NOT NULL,key TEXT NOT NULL,
                md5 TEXT,added_at TEXT NOT NULL,removed_at TEXT,UNIQUE(namespace,key));
            INSERT INTO term_assets VALUES (1,'library','old',NULL,'2026-01-01 00:00:00','2026-01-01 00:00:01');
            """);c.commit()
        before=database(actual)
        shutil.copyfile(actual,expected)
        now=int(time.time())
        clock=lambda offset:datetime.datetime.fromtimestamp(now+offset,datetime.timezone.utc).strftime('%Y-%m-%d %H:%M:%S')
        for path,offset in ((actual,0),(expected,-1)):
            with closing(sqlite3.connect(path)) as c:
                c.execute("INSERT INTO term_assets VALUES (2,'library','new',NULL,?,?)",(clock(offset),clock(offset)))
                c.commit()
        self.equivalent(actual,expected,before,recorded=False)
        saved=self.root/'clock-good.sqlite';shutil.copyfile(actual,saved)
        for name,sql,values in (
            ('outside 60 seconds',"UPDATE term_assets SET removed_at=? WHERE id=2",(clock(-61),)),
            ('NULL differs',"UPDATE term_assets SET removed_at=NULL WHERE id=2",()),
            ('existing clock differs',"UPDATE term_assets SET removed_at=? WHERE id=1",(clock(0),)),
            ('other cell differs',"UPDATE term_assets SET key='different' WHERE id=2",()),
            ('rowid differs',"UPDATE term_assets SET id=3 WHERE id=2",()),
        ):
            with self.subTest(name=name):
                shutil.copyfile(saved,actual)
                with closing(sqlite3.connect(actual)) as c:
                    c.execute(sql,values);c.commit()
                with self.assertRaises(AssertionError):
                    self.equivalent(actual,expected,before,recorded=False)

    def test_all_application_versions_match_python(self):
        for version in range(1,36):
            with self.subTest(version=version):
                lib=self.package(version);before=database(lib.db),database(lib.source)
                expected=self.baseline(lib)
                self.run_native(lib)
                self.equivalent(lib.db,expected.db,before[0])
                self.assertEqual(database(lib.source),database(expected.source))
                backups=list((lib.path/'library-backups').glob('application-before-*'))
                self.assertEqual(len(backups),1)
                self.assertEqual(database(backups[0]/'library.sqlite'),before[0]);self.assertEqual(database(backups[0]/'source.sqlite'),before[1])
                # Explicit backup restore on a quiescent, test-owned synthetic clone.
                restore=self.root/f'restored-{version}.pilib';shutil.copytree(backups[0],restore)
                self.assertEqual(database(restore/'library.sqlite'),before[0]);self.assertEqual(database(restore/'source.sqlite'),before[1])

    def test_failure_leaves_both_original_databases_and_retains_backup(self):
        lib=self.package(35)
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute("CREATE TRIGGER injected BEFORE INSERT ON meta WHEN NEW.key='schema_version' AND NEW.value='36' BEGIN SELECT RAISE(ABORT,'synthetic-upgrade-failure'); END");c.commit()
        before=database(lib.db),database(lib.source),(lib.path/'library.json').read_bytes()
        log=self.run_native(lib,False);self.assertIn('synthetic-upgrade-failure',log)
        self.assertEqual((database(lib.db),database(lib.source),(lib.path/'library.json').read_bytes()),before)
        backups=list((lib.path/'library-backups').glob('application-before-*'));self.assertEqual(len(backups),1)
        self.assertEqual(database(backups[0]/'library.sqlite'),before[0]);self.assertEqual(database(backups[0]/'source.sqlite'),before[1])
        # Restore the complete backup pair after a synthetic subsequent corruption.
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute("UPDATE meta SET value='999' WHERE key='schema_version'");c.commit()
        for filename in ('library.sqlite','source.sqlite','library.json'):
            shutil.copyfile(backups[0]/filename,lib.path/filename)
        self.assertEqual((database(lib.db),database(lib.source),(lib.path/'library.json').read_bytes()),before)

    def test_source_only_upgrade_of_current_core_is_explicit_and_idempotent(self):
        lib=self.package(36)
        expected=self.baseline(lib)
        # Python upgraded the source; use its complete core as the current-core input,
        # retaining the genuinely old source on the native side.
        shutil.copyfile(expected.db,lib.db)
        before=database(lib.db),database(lib.source)
        self.run_native(lib)
        self.assertEqual(database(lib.db),before[0])
        self.assertEqual(database(lib.source),database(expected.source))
        saved=list((lib.path/'library-backups').glob('application-before-*'))
        self.assertEqual(len(saved),1)
        self.run_native(lib)
        self.assertEqual(list((lib.path/'library-backups').glob('application-before-*')),saved)
        self.assertEqual(database(lib.db),before[0])

    def test_v2_retirement_matches_python_without_losing_human_verdicts(self):
        from tests.test_native_library_upgrade import OLD_IDENTITY
        for version in (25,36,34):
            with self.subTest(version=version):
                lib=self.package(version)
                with closing(sqlite3.connect(lib.db)) as c:
                    c.executescript(OLD_IDENTITY)
                    c.execute("INSERT INTO meta VALUES ('identity_policy','v2')");c.commit()
                if version==34:
                    migrated=subprocess.run([str(self.binary),'migrate','--db',str(lib.db)],env=self.env,capture_output=True,timeout=30)
                    self.assertEqual(migrated.returncode,0,migrated.stderr)
                    with closing(sqlite3.connect(lib.db)) as c:
                        c.executescript(OLD_IDENTITY.replace("INSERT INTO meta VALUES", "INSERT OR REPLACE INTO meta VALUES"));c.commit()
                before=database(lib.db)
                expected=self.baseline(lib);self.run_native(lib)
                self.equivalent(lib.db,expected.db,before)
                with closing(sqlite3.connect(lib.db)) as c:
                    self.assertEqual(c.execute("SELECT value FROM meta WHERE key='identity_policy'").fetchone(),('hybrid',))
                    self.assertEqual(c.execute('SELECT count(*) FROM assertions').fetchone(),(1,))

    def test_old_application_history_physical_rows_and_undo_survive_upgrade(self):
        # Real legacy application action format, including an already undone row.
        # Check source and application separately; runtime undo commits only app.
        for version in (17, 25, 35, 36):
            with self.subTest(version=version):
                lib=self.package(version)
                with closing(sqlite3.connect(lib.db)) as c:
                    history=c.execute('SELECT rowid,* FROM actions ORDER BY id').fetchall()
                source_before=database(lib.source)
                self.run_native(lib)
                with closing(sqlite3.connect(lib.db)) as c:
                    self.assertEqual(c.execute('SELECT rowid,* FROM actions ORDER BY id').fetchall(),history)
                    self.assertEqual(c.execute('SELECT rowid,* FROM retained_rows').fetchall(),[(27,'preserve')])
                with closing(sqlite3.connect(lib.source)) as c:
                    self.assertEqual(c.execute('SELECT rowid,* FROM retained_source_rows').fetchall(),[(83,b'\x00\x01\xff')])
                backups=list((lib.path/'library-backups').glob('application-before-*'))
                self.assertEqual(len(backups),1)
                self.assertEqual(database(backups[0]/'source.sqlite'),source_before)
                with closing(sqlite3.connect(backups[0]/'library.sqlite')) as c:
                    self.assertEqual(c.execute('SELECT rowid,* FROM actions ORDER BY id').fetchall(),history)
                source_upgraded=database(lib.source)
                port=free_port();origin=f'http://127.0.0.1:{port}'
                log=tempfile.TemporaryFile();self.addCleanup(log.close)
                process=subprocess.Popen([str(self.binary),'http','--no-sync','--no-names',
                    '--library',str(lib.path),'--port',str(port),'--root',str(self.resources)],
                    env={**self.env,'PATH':'/nonexistent'},stdout=log,stderr=log, **SPAWN)
                def stop(p=process):
                    if p.poll() is None:
                        p.terminate();p.wait(5)
                self.addCleanup(stop)
                wait_until(lambda:fetch(origin,{'url':'/api/bootstrap'},origin)['status']==200,timeout=8)
                token=json.loads(fetch(origin,{'url':'/api/bootstrap'},origin)['body'])['token']
                response=fetch(origin,{'url':'/api/undo','method':'POST','body':b'{"id":57}',
                    'headers':{'Content-Type':'application/json','X-CSRF-Token':token}},origin)
                self.assertEqual(response['status'],200,response['body'])
                stop()
                with closing(sqlite3.connect(lib.db)) as c:
                    rows=c.execute('SELECT rowid,* FROM actions ORDER BY id').fetchall()
                    expected=[tuple(row[:-1])+(1,) if row[0]==57 else row for row in history]
                    self.assertEqual(rows,expected)
                    self.assertEqual(c.execute("SELECT * FROM prefs WHERE key='identity_skipped'").fetchall(),[])
                    self.assertEqual(c.execute('SELECT rowid,* FROM retained_rows').fetchall(),[(27,'preserve')])
                self.assertEqual(database(lib.source),source_upgraded)

    def test_captured_sql_still_matches_the_python_reference(self):
        # The former Python migration SQL constants are recorded; the captured files must keep them.
        p=self.resources/'native/terms/src/app_migration_sql'
        recorded=self.rec.value('python migration sql')
        artists=recorded.pop('artists')
        for name,sql in recorded.items():
            self.assertEqual((p/(name+'.sql')).read_text(),'-- Existing Python migration SQL; keep rules unchanged.\n'+sql+';\n')
        self.assertEqual((p/'artists.sql').read_text(),artists+';\n')

    def test_missing_general_initialization_imports_wd_with_identical_content_fingerprint(self):
        lib=self.package(35)
        with closing(sqlite3.connect(lib.source)) as c:
            c.execute('ALTER TABLE labels ADD COLUMN ts TEXT')
            c.execute("INSERT INTO labels VALUES ('a','general','solo',0.99,'2026-01-01 00:00:00')");c.commit()
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute("INSERT INTO general_hits VALUES ('a','solo')")
            c.execute("DELETE FROM meta WHERE key IN ('general_indexed','rating_indexed')");c.commit()
        before=database(lib.db)
        expected=self.baseline(lib)
        self.run_native(lib)
        self.equivalent(lib.db,expected.db,before)
        with closing(sqlite3.connect(lib.db)) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM term_evidence WHERE source='wd'").fetchone(),(1,))
            self.assertEqual(c.execute("SELECT value FROM general_hits WHERE uuid='a'").fetchall(),[('solo',)])

    def test_unindexed_source_builds_the_same_initial_core_refresh(self):
        lib=self.package(36)
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute("DELETE FROM meta WHERE key IN ('indexed','general_indexed','rating_indexed')");c.commit()
        # Preserve the input before the native migration; bind only the two reference birth UUIDs.
        expected_path=self.root/'unindexed-python.pilib';shutil.copytree(lib.path,expected_path)
        before=database(lib.db)
        self.run_native(lib)
        with closing(sqlite3.connect(lib.db)) as c:
            ids=dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
            indexed=c.execute("SELECT value FROM meta WHERE key='indexed'").fetchone()[0]
        expected=self.rec.package('unindexed-python',expected_path)
        self.equivalent(lib.db,expected.db,before)
        self.assertEqual(database(lib.source),database(expected.source))

    def test_photos_source_schema_and_presets_match_optional_read_only_lookup(self):
        for version,available in ((35,True),(36,False)):
            with self.subTest(photos_available=available):
                lib=self.package(version)
                photos=self.root/f'photos-{version}'
                if available:
                    (photos/'database').mkdir(parents=True)
                    with closing(sqlite3.connect(photos/'database/Photos.sqlite')) as c:
                        c.executescript("""CREATE TABLE ZASSET(ZUUID TEXT,Z_PK INTEGER,ZWIDTH INTEGER,ZHEIGHT INTEGER,ZADDEDDATE REAL,ZHIDDEN INTEGER,ZDIRECTORY TEXT,ZFILENAME TEXT,ZADDITIONALATTRIBUTES INTEGER,ZKIND INTEGER,ZTRASHEDSTATE INTEGER);
                        CREATE TABLE ZADDITIONALASSETATTRIBUTES(Z_PK INTEGER,ZIMPORTEDBYBUNDLEIDENTIFIER TEXT,ZORIGINALSTABLEHASH TEXT,ZORIGINALFILESIZE INTEGER);
                        INSERT INTO ZASSET VALUES ('A',1,10,20,0,0,'','synthetic',1,0,0);
                        INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES (1,'synthetic.bundle',NULL,0);""");c.commit()
                lib.config['sources']=[{'kind':'photos','id':'s1','path':str(photos),'apps':{'synthetic.bundle':'pixiv'}}]
                (lib.path/'library.json').write_text(json.dumps(lib.config))
                with closing(sqlite3.connect(lib.source)) as c:
                    c.execute('UPDATE assets SET uuid=upper(uuid)');c.commit()
                before=database(lib.db)
                expected=self.baseline(lib);self.run_native(lib)
                self.equivalent(lib.db,expected.db,before)
                self.assertEqual(database(lib.source),database(expected.source))

    def test_missing_application_version_uses_the_early_python_branch(self):
        lib=self.package(1)
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute("DELETE FROM meta WHERE key='schema_version'");c.commit()
        before=database(lib.db);expected=self.baseline(lib);self.run_native(lib)
        self.equivalent(lib.db,expected.db,before)
        self.assertEqual(database(lib.source),database(expected.source))

    def test_album_title_alias_and_mapping_are_preserved(self):
        lib=self.package(35)
        mapping={'map':{'album-key':{'char_id':'alice'}}}
        (lib.path/'album-char-map.json').write_text(json.dumps(mapping))
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('ccip','alice','user-character','import')");c.commit()
        with closing(sqlite3.connect(lib.source)) as c:
            c.execute("INSERT INTO decisions VALUES ('a','character','album-key','album: 相册标题 pk=1')");c.commit()
        before=database(lib.db);expected=self.baseline(lib);self.run_native(lib)
        self.equivalent(lib.db,expected.db,before)
        self.assertEqual(json.loads((lib.path/'album-char-map.json').read_text()),mapping)
        with closing(sqlite3.connect(lib.db)) as c:
            self.assertEqual(c.execute("SELECT value FROM names WHERE source='album'").fetchall(),[('相册标题',)])

    def test_empty_application_builds_initial_core_without_python(self):
        lib=self.package(36)
        lib.db.unlink()
        sqlite3.connect(lib.db).close()
        before=database(lib.db)
        expected_path=self.root/'empty-python.pilib';shutil.copytree(lib.path,expected_path)
        self.run_native(lib)
        # Reference CCIP birth IDs are explicit approved inputs to both producers.
        with closing(sqlite3.connect(lib.db)) as c:
            ids=dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
            indexed=c.execute("SELECT value FROM meta WHERE key='indexed'").fetchone()[0]
        expected=self.rec.package('empty-python',expected_path)
        self.equivalent(lib.db,expected.db,before)
        self.assertEqual(database(lib.source),database(expected.source))

    def test_works_variants_and_mixed_name_sources_match_python(self):
        lib=self.package(25)
        with closing(sqlite3.connect(lib.db)) as c:
            c.executescript("""
            INSERT INTO entities(id,name,kind) VALUES ('example-work','','work');
            INSERT INTO entities(id,name,kind,parent_id,variant) VALUES ('user-skin','','skin','user-character','summer');
            UPDATE entities SET work='example_work' WHERE id IN ('user-character','user-skin');
            INSERT INTO links(system,key,entity_id,source) VALUES ('qualifier','example_work','example-work','import'),('wd','alice_tag','user-character','import');
            INSERT INTO display_names VALUES ('character','alice_tag','ja','model','日本語'),('work','example_work','zh','user','作品');
            INSERT INTO name_aliases VALUES ('character','alice_tag','画师别名',10001),('work','example_work','旧作品名',1);
            INSERT INTO assertions VALUES ('b','user-skin','confirmed','user');
            """);c.commit()
        before=database(lib.db);expected=self.baseline(lib);self.run_native(lib)
        self.equivalent(lib.db,expected.db,before)
        self.assertEqual(database(lib.source),database(expected.source))

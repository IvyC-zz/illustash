"""用合成信箱，对照未改动的旧算法测试作品集验证。"""
import io
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time
import urllib.parse
import unittest
from tests.provenance_expectations import verify
from tests.provenance_expectations import Stopped
from tests import test_native_provenance_match as match_driver


class Gallery:
    authorized=True
    def __init__(self,image,mode='both',works=2,refuse=None,match=True):
        self.image,self.mode,self.works,self.refuse,self.match=image,mode,works,refuse,match
        self.used=0;self.calls=[]

    def get(self,url,params=None,raw=False,extra=None):
        parts=urllib.parse.urlsplit(url)
        query=urllib.parse.parse_qs(parts.query)
        if params:
            for key,value in dict(params).items():query.setdefault(key,[]).append(str(value))
        self.calls.append((parts.netloc,parts.path,raw))
        if self.refuse and self.refuse in url:raise Stopped('synthetic refused host')
        if raw:
            self.used+=len(self.image)
            return self.image if self.match else None
        if parts.path.endswith('posts.json'):
            return [dict(id=i,image_width=64,image_height=48,preview_file_url=f'https://cdn.donmai.us/{i}.png',source='') for i in range(self.works)] if self.mode in ('danbooru','both') else []
        if parts.path.endswith('artists.json'):
            return [dict(urls=[dict(url='https://www.pixiv.net/users/12')])] if self.mode=='both' else []
        if parts.path.endswith('tags.json'):return [dict(name='synthetic',post_count=1000)]
        if parts.path.endswith('profile/all'):return dict(body=dict(illusts={str(i):None for i in range(self.works)},manga={}))
        if parts.path.endswith('profile/illusts'):
            # parse_qs 保留重复的 ids[]；旧的 kwargs 是元组列表。
            if params:ids=[v for k,v in params if k=='ids[]']
            else:ids=query.get('ids[]',[])
            return dict(body=dict(works={str(i):dict(id=str(i),url=f'https://i.pximg.net/img/2026/01/01/00/00/00/{i}_p0.jpg',pageCount=1) for i in ids}))
        raise AssertionError('unexpected synthetic URL '+url)


class NativeProvenanceVerifyTest(unittest.TestCase):
    setUpClass=classmethod(match_driver.NativeProvenanceMatchTest.setUpClass.__func__)
    setUp=match_driver.NativeProvenanceMatchTest.setUp
    fixture_local=match_driver.NativeProvenanceMatchTest.fixture_local
    fixture_match=match_driver.NativeProvenanceMatchTest.fixture_match
    rows=match_driver.NativeProvenanceMatchTest.rows

    def fixture_verify(self,mode='both',works=2,refuse=None,weak=False,explicit=True,match=True):
        request,reference=self.fixture_match()
        request['step']='verify';request['network']=True;request['artists']=['A'] if explicit else []
        image=Path(request['library'])/'generated.png'
        for cache in (reference.path,Path(request['cache'])):
            with sqlite3.connect(cache/'provenance.sqlite') as c:
                c.execute('DELETE FROM ref_sigs')
                c.execute("INSERT INTO artist_links(system,key,artist_id,url,source) VALUES ('danbooru','synthetic','A','','test')")
                if mode=='pixiv':c.execute("INSERT INTO artist_links(system,key,artist_id,url,source) VALUES ('pixiv','12','A','','test')")
                c.execute("INSERT INTO seeds(artist_id,source) VALUES ('A',?)",('ocr' if weak else 'pixel_hash',))
        return request,reference,lambda:Gallery(image.read_bytes(),mode,works,refuse,match)

    def probe_broker(self,request,net):
        incoming,output=self.root/'request.json',self.root/'output.json'
        incoming.write_text(json.dumps(request),encoding='utf-8');output.unlink(missing_ok=True)
        mailbox=Path(request['mailbox']);calls=mailbox/'net-request.json'
        response=mailbox/'net-response.json';body=mailbox/'net-body.bin'
        with tempfile_file(self.root/'stderr') as err:
            child=subprocess.Popen([str(self.binary),str(incoming),str(output)],env=os.environ|{'PATH':''},stdout=subprocess.DEVNULL,stderr=err,start_new_session=os.name!='nt')
            self.addCleanup(lambda:child.kill() if child.poll() is None else None)
            deadline=time.monotonic()+60
            while child.poll() is None:
                if time.monotonic()>deadline:
                    child.kill();child.wait();self.fail('synthetic broker deadline')
                if calls.is_file():
                    try:
                        call=json.loads(calls.read_text(encoding='utf-8'))
                    except PermissionError:
                        # NTFS 改名/删除的共享冲突可能在存在性检查之后仍未解除。
                        # 一直被拒绝时，外层的期限仍会让测试失败。
                        time.sleep(.002)
                        continue
                    self.assertEqual(call['identity'],request['identity'])
                    parsed=urllib.parse.urlsplit(call['url']);raw=parsed.netloc in ('cdn.donmai.us','i.pximg.net')
                    try:
                        data=net.get(call['url'],raw=raw)
                        answer=dict(status=200 if data is not None else 404,used=net.used)
                        body.write_bytes(data if raw and data is not None else json.dumps(data).encode())
                    except Stopped as e:answer=dict(error=str(e),used=net.used)
                    calls.unlink()
                    temporary=response.with_suffix('.tmp');temporary.write_text(json.dumps(answer));temporary.replace(response)
                else:time.sleep(.002)
            child.wait()
        self.assertEqual(child.returncode,0,(self.root/'stderr').read_text())
        return json.loads(output.read_text())

    def all_rows(self,path):
        result=self.rows(path)
        with sqlite3.connect(path/'provenance.sqlite') as c:
            for table,columns in dict(ref_sigs='site,post_id,page,artist_id,ratio,grid,colour,url',artist_links='system,key,artist_id,url,source').items():
                result[table]=c.execute(f'SELECT {columns} FROM {table} ORDER BY 1,2,3').fetchall()
        return result

    def parity(self,request,reference,factory):
        core=reference.db.read_bytes();source=reference.source.read_bytes()
        expected_net=factory();actual_net=factory()
        expected=verify.run(reference,expected_net,artists=request['artists'] or None,log=lambda _:None)
        actual=self.probe_broker(request,actual_net)
        self.assertEqual(actual['result'],expected)
        self.assertEqual(actual['identity'],request['identity'])
        self.assertEqual(self.all_rows(Path(request['cache'])),self.all_rows(reference.path))
        self.assertEqual(reference.db.read_bytes(),core);self.assertEqual(reference.source.read_bytes(),source)
        self.assertEqual(sorted(actual_net.calls),sorted(expected_net.calls))
        return actual['result'],actual_net

    def test_danbooru_pixiv_discovery_exact_receipts_cached_rerun(self):
        request,reference,factory=self.fixture_verify()
        result,_=self.parity(request,reference,factory)
        self.assertEqual(result,dict(artists=1,matched_artists=1,matches=4,cold=0,stopped=''))
        _,net=self.parity(request,reference,factory)
        self.assertFalse(any(raw for _,_,raw in net.calls))

    def test_pixiv_refusal_preserves_danbooru_and_leaves_artist_unfinished(self):
        request,reference,factory=self.fixture_verify(refuse='profile/all')
        result,_=self.parity(request,reference,factory)
        self.assertEqual((result['matches'],result['stopped']),(2,'synthetic refused host'))
        self.assertFalse(self.rows(Path(request['cache']))['lookups'])

    def test_parent_refusal_stops_at_correct_stage_without_miss(self):
        request,reference,factory=self.fixture_verify(refuse='posts.json')
        result,_=self.parity(request,reference,factory)
        self.assertEqual(result['artists'],0)
        self.assertEqual(result['stopped'],'synthetic refused host')
        self.assertFalse(self.rows(Path(request['cache']))['lookups'])

    def test_weak_lead_no_match_stops_at_probe_and_records_probe_cursor(self):
        request,reference,factory=self.fixture_verify(mode='danbooru',works=35,weak=True,match=False)
        result,net=self.parity(request,reference,factory)
        self.assertEqual(result['matches'],0)
        self.assertEqual(sum(raw for _,_,raw in net.calls),30)
        with sqlite3.connect(Path(request['cache'])/'provenance.sqlite') as c:
            self.assertEqual(c.execute("SELECT cursor FROM lookups WHERE backend='verify'").fetchone()[0],'probe')

    def test_pixiv_two_listings_ahead_and_weak_matching_widening(self):
        request,reference,factory=self.fixture_verify(mode='pixiv',works=100,weak=True)
        result,_=self.parity(request,reference,factory)
        # 旧版即使弱计划从 30 开始，切片也按 48 前进；
        # 保留它的扩展/超出行为，不悄悄改进。
        self.assertEqual(result['matches'],36)

    def test_automatic_popularity_selection_and_done_skip(self):
        request,reference,factory=self.fixture_verify(mode='danbooru',explicit=False)
        result,_=self.parity(request,reference,factory)
        self.assertEqual(result['artists'],1)
        result,net=self.parity(request,reference,factory)
        self.assertEqual(result['artists'],0)
        self.assertFalse(net.calls)

    def test_popularity_refusal_does_not_turn_cold_lead_into_miss(self):
        request,reference,factory=self.fixture_verify(mode='danbooru',explicit=False,refuse='tags.json')
        result,_=self.parity(request,reference,factory)
        self.assertEqual((result['artists'],result['cold'],result['stopped']),(0,1,''))
        self.assertFalse(self.rows(Path(request['cache']))['lookups'])


# 用具名辅助函数，免得 unittest 通过导入发现另一个 TestCase。
def tempfile_file(path):return path.open('wb')

"""IQDB 网络收据直接验收原生协调器；队列、优先级和分级规则在 Rust 测试中保留。"""
import unittest
from tests import test_native_provenance as fixture


class IqdbTest(unittest.TestCase):
    # 共用历史合成包输入，不增加新的参考输出或可重录入口。
    __module__ = 'tests.test_native_provenance'
    setUpClass = classmethod(fixture.NativeProvenanceTest.setUpClass.__func__)
    def setUp(self):
        fixture.NativeProvenanceTest.setUp(self)
        source = fixture.WORKER.replace("result=[{'asset':x,'hash':'a'*32 if step=='pixel_hash' else 'iqdb_'+'a'*528} for x in ids]",
                                        "result=[] if step=='pixel_hash' else [{'asset':x,'hash':'iqdb_'+'a'*528} for x in ids]")
        self.worker.write_text('#!'+fixture.sys.executable+'\n'+source)

    def test_hits_are_recorded_with_their_upstream_work(self):
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertEqual(set(self.sql("SELECT site,post_id,method FROM image_sources WHERE asset='a'",cache=True)),
                         {('danbooru','2','iqdb'),('pixiv','2','iqdb')})
        self.assertEqual(self.sql("SELECT key FROM artist_links WHERE system='danbooru'",cache=True),[('synthetic_other',)])
        self.assertEqual(self.sql("SELECT source FROM seeds",cache=True),[('iqdb',)])
        self.assertEqual(self.sql("SELECT last_result FROM lookups WHERE backend='iqdb'",cache=True),[('hit',)])
        self.assertEqual(len(self.calls),1)

    def test_a_refusal_stops_danbooru_for_the_run(self):
        self.code=429
        self.post({'action':'lookup','ids':['a','b']});value=self.finish('stopped')
        self.assertIn('429',str(value))
        self.assertEqual(len(self.calls),1)
        self.assertEqual(self.sql("SELECT count(*) FROM lookups WHERE backend='iqdb'",cache=True),[(0,)])

    def test_not_without_authorisation(self):
        self.post({'action':'start'});self.finish('stopped')
        self.assertEqual(self.calls,[])


# Only helpers are reused; the original class's test methods are discovered in its own module.
for base in reversed(fixture.NativeProvenanceTest.__mro__):
    if base in (unittest.TestCase,object):continue
    for name,value in vars(base).items():
        if not name.startswith('test_') and not name.startswith('__') and name not in vars(IqdbTest):
            setattr(IqdbTest,name,value)

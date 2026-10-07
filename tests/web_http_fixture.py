"""Reference HTTP server fixture (former tests/test_web_http.py base class), disposable data only."""
import json
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from PIL import Image
from web.server import Server
from web.store import Store
from tests.synthetic_source import source_fixture as fixture


class HTTPTest(unittest.TestCase):
    """Reference loopback server on a disposable library; subclasses add their own tests."""
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();root=Path(self.temp.name)
        source=fixture(root)
        Image.new('RGB',(60,90),'#8da195').save(root/'a.png')
        if self._testMethodName == 'test_identity_count_preserves_queue_filter_skip_and_undo':
            with sqlite3.connect(source) as c:
                c.execute("INSERT INTO labels VALUES ('b','rating','explicit',1)")
        self.store=Store(source,root/'app.sqlite',identity='hybrid' if 'identity' in self._testMethodName else None);self.store.refresh()
        self.server=Server(('127.0.0.1',0),self.store,root/'cache')
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
        self.url=self.server.origin

    def tearDown(self):
        self.server.shutdown();self.thread.join();self.server.server_close();self.temp.cleanup()

    def request(self,path,body=None,headers=None):
        h={}
        if body is not None:h={'Content-Type':'application/json','Origin':self.url,'X-CSRF-Token':self.server.token}
        h.update(headers or {})
        req=urllib.request.Request(self.url+path, data=json.dumps(body).encode() if body is not None else None,headers=h)
        try:
            with urllib.request.urlopen(req,timeout=5) as r:return r.status,r.read(),r.headers
        except urllib.error.HTTPError as e:return e.code,e.read(),e.headers

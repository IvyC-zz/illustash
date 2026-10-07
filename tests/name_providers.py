"""Fixed text answers of the name providers (Danbooru wiki, Bangumi, model API); never the network."""
import copy
import json
import urllib.parse

from web.names import MODEL_BASE

DS = MODEL_BASE + '/chat/completions'


class Providers:
    def __init__(self):
        self.calls = []
        self.wikis = {'Example': ['エグザンプル'], 'alice': ['ア_リス', '愛麗絲'], 'bob': ['ボブ']}
        self.subjects = [{'id': 99, 'name': 'エグザンプル外伝', 'name_cn': '错误作品'},
                         {'id': 1, 'name': 'エグザンプル', 'name_cn': '示例作品'}]
        self.characters = [{'id': 10, 'name': 'アリス'}, {'id': 11, 'name': '別人'}]
        self.character_fields = {10: {'name': 'アリス', 'infobox': [{'key': '简体中文名', 'value': '爱丽丝'}]}}
        self.translation = {'zh': '译名'}

    def __call__(self, url, body=None, key=None):
        self.calls.append((url, copy.deepcopy(body), key))
        if 'wiki_pages.json' in url:
            tag = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)['search[title]'][0]
            return [{'title': tag, 'other_names': self.wikis[tag]}] if tag in self.wikis else []
        if '/search/subjects' in url:
            return {'data': self.subjects, 'total': len(self.subjects)}
        if '/search/characters' in url:
            return {'data': [r | self.character_fields.get(r['id'], {}) for r in self.characters]}
        if '/characters/' in url and url.endswith('/subjects'):
            return [{'name': 'エグザンプル', 'name_cn': '示例作品'}]
        if url == DS:
            return {'choices': [{'message': {'content': json.dumps(self.translation, ensure_ascii=False)}}]}
        raise AssertionError('Unexpected endpoint')

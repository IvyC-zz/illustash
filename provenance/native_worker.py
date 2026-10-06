#!/usr/bin/env python3
"""Optional computation adapter for illustash-provenance/1.

Only machine cache is writable. Networking is requested through the native parent's
scoped file mailbox; this adapter never discovers credentials or invokes provenance.run.
Configure this executable explicitly as ILLUSTASH_PROVENANCE_WORKER.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import signal
import threading
import time
from types import SimpleNamespace
import urllib.parse
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))


def write(path, value):
    with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=path.parent, delete=False) as stream:
        temp = Path(stream.name)
        json.dump(value, stream, ensure_ascii=False)
    try:
        temp.replace(path)
    finally:
        temp.unlink(missing_ok=True)


class Network:
    """The computation worker can request only the parent's read-only query allowlist."""
    def __init__(self, request):
        self.request = request
        self.mailbox = Path(request['mailbox'])
        self.lock = threading.Lock()
        self.authorized = request['network']
        self.used = 0
        self.stopped = {}

    def get(self, url, params=None, raw=False, extra=None):
        from provenance.net import Stopped
        if params:
            url += ('&' if '?' in url else '?') + urllib.parse.urlencode(params)
        with self.lock:
            output = self.mailbox / 'net-response.json'
            output.unlink(missing_ok=True)
            write(self.mailbox / 'net-request.json', {'identity': self.request['identity'], 'url': url})
            while not output.exists():
                time.sleep(.01)
            response = json.loads(output.read_text())
            self.used = response['used']
            if 'error' in response:
                raise Stopped(response['error'])
            if response['status'] in (401, 404):
                return None
            body = (self.mailbox / 'net-body.bin').read_bytes()
            return body if raw else json.loads(body)


def execute(request):
    from provenance import assets, db
    from provenance.sources import pixel_hash
    path = Path(request['library'])
    lib = SimpleNamespace(path=Path(request['cache']), db=path / 'library.sqlite',
                          source=path / 'source.sqlite', config=json.loads((path / 'library.json').read_text()))
    step = request['step']
    mailbox = Path(request['mailbox'])
    counter = 0

    def log(text):
        nonlocal counter
        counter += 1
        write(mailbox / 'progress.json', {'log': str(text)[:200], 'sequence': counter})
    log.progress = lambda **numbers: write(mailbox / 'progress.json', {'progress': numbers, 'sequence': time.monotonic_ns()})
    log('开始本地计算')
    if step == 'pixel_hash':
        with db.connect(lib) as c:
            done = {r[0] for r in c.execute("SELECT target FROM lookups WHERE backend='pixel_hash' AND last_result IN ('hit','miss')")}
        only = set(request['only']) if request['only'] else None
        todo = [p for p in assets.pictures(lib) if p['asset'] not in done and (only is None or p['asset'] in only)]
        rows = []
        for picture in todo:
            _, digest = pixel_hash._hash((picture['asset'], assets.original(lib, picture)))
            rows.append({'asset': picture['asset'], 'hash': digest})
            log.progress(done=len(rows), total=len(todo))
        return rows
    if step == 'iqdb':
        from provenance.sources import iqdb
        with db.connect(lib) as c:
            todo = iqdb.order(lib, c, request['only'] or None)[:iqdb.PER_RUN]
        binary = os.environ.get('ILLUSTASH_IQDB_BINARY') or os.environ.get('ILLUSTASH_IQDB_BIN')
        if not binary:
            return {'failed': 1, 'stopped': '未配置本地 IQDB 签名计算程序'}
        lines = ''.join(json.dumps({'asset': p['asset'], 'path': str(assets.original(lib, p) or p['src_path'])}) + '\n' for p in todo)
        # Stream signatures so a long local decode batch keeps the native idle
        # heartbeat alive; an input file avoids pipe deadlock while stdout fills.
        input_path = mailbox / 'hash-input.jsonl'
        input_path.write_text(lines, encoding='utf-8')
        rows = []
        with input_path.open() as incoming, subprocess.Popen(
                [binary, '--hash-only', '1'], stdin=incoming, stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL, text=True) as child:
            for line in child.stdout:
                rows.append(json.loads(line))
                log.progress(done=len(rows), total=len(todo))
            if child.wait():
                raise RuntimeError('IQDB signature calculation failed')
        return rows
    if step == 'local_match':
        from provenance.verify import local_run as run
    elif step == 'verify':
        from provenance.verify import run
    elif step == 'groups':
        from provenance.sources.groups import run
    else:
        raise ValueError('unknown computation step')
    kwargs = {'artists': request['artists']} if step == 'verify' and request['artists'] else {}
    return run(lib, Network(request), log=log, **kwargs)


def main():
    request_path, response_path = map(Path, sys.argv[1:])
    request = json.loads(request_path.read_text())
    if request['format'] != 'illustash-provenance/1':
        raise ValueError('unsupported format')
    def watch_owner():
        while os.getppid() == request['owner_pid']:
            time.sleep(.25)
        os.killpg(os.getpgrp(), signal.SIGKILL)
    if os.name != 'nt':
        threading.Thread(target=watch_owner, daemon=True).start()
    result = execute(request)
    if isinstance(result, list):
        write(Path(request['mailbox']) / 'hashes.json', result)
        result = {'hashes': 'hashes.json'}
    write(response_path, {'format': request['format'], 'identity': request['identity'], 'step': request['step'], 'result': result})


if __name__ == '__main__':
    main()

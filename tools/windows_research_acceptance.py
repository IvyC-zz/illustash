"""Verify a relocated research artifact on generated images with the bundled runtime.

No real library/accounts, no third-party requests. The harness uses build-time Python;
all product workers use the artifact's embedded interpreter. --work must not exist.
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import time
from urllib.request import Request, urlopen
from PIL import Image, ImageDraw, ImageFont


def run(artifact, work):
    artifact = artifact.resolve(); work.mkdir(parents=True, exist_ok=False)
    folder = work/'generated';folder.mkdir()
    font = ImageFont.truetype(str(Path(os.environ['SystemRoot'])/'Fonts/arial.ttf'),38)
    for i in range(3):
        image=Image.new('RGB',(900,160),(255,255,255-i))
        ImageDraw.Draw(image).text((20,25),'@synthetic pixiv.net/users/123456',fill='black',font=font)
        image.save(folder/f'合成-{i}.png')
    library=work/'synthetic.pilib'
    env={**os.environ, 'PATH':os.environ['SystemRoot']+'/System32', 'HOME':str(work/'home'),
         'USERPROFILE':str(work/'home'), 'APPDATA':str(work/'support'), 'LOCALAPPDATA':str(work/'local'),
         'ILLUSTASH_APP_SUPPORT':str(work/'support'), 'ILLUSTASH_SHARED':str(work/'shared'),
         'ILLUSTASH_WORKER_PYTHON':str(artifact/'runtime/python.exe')}
    # One installed worker serves all four kinds; the host (and --check here) names the kind.
    for kind in ('research','index','provenance','recognition'):
        env['ILLUSTASH_'+kind.upper()+'_WORKER']=str(artifact/'bin/illustash-worker.exe')
    env.pop('ILLUSTASH_IQDB_BINARY',None);env.pop('ILLUSTASH_IQDB_BIN',None);env.pop('ILLUSTASH_WORKER_KIND',None)
    core=artifact/'bin/illustash-terms.exe'
    for kind in ('research','index','provenance','recognition'):
        subprocess.run([env['ILLUSTASH_'+kind.upper()+'_WORKER'],'--check'],env={**env,'ILLUSTASH_WORKER_KIND':kind},check=True)
    subprocess.run([str(core),'library-create','--folder',str(folder),'--dest',str(library),'--name','Synthetic'],env=env,check=True)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    origin=f'http://127.0.0.1:{port}';token=''
    def request(path, data=None):
        payload=None if data is None else json.dumps(data).encode('utf-8')
        req=Request(origin+path,payload,headers={'Origin':origin,'Content-Type':'application/json','X-CSRF-Token':token})
        with urlopen(req,timeout=30) as response:return json.load(response)
    def wait(check, seconds=120):
        until=time.monotonic()+seconds
        while time.monotonic()<until:
            try:
                value=check()
                if value:return value
            except (ConnectionError,OSError):pass
            time.sleep(.1)
        raise AssertionError('installed artifact did not complete; inspect '+str(work/'host.log'))
    with (work/'host.log').open('wb') as log:
        host=subprocess.Popen([str(core),'http','--native','--library',str(library),'--port',str(port),
                               '--no-open','--no-names','--no-index-check','--no-auto-sync'],env=env,
                              creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,stdout=log,stderr=log)
        try:
            bootstrap=wait(lambda:request('/api/bootstrap'));token=bootstrap['token']
            wait(lambda:request('/api/similar')['state']['status']=='idle')
            start=time.monotonic();request('/api/sync',{})
            def synced():
                state=request('/api/sync')['state']
                if state['status']=='error':raise AssertionError(state)
                return state if state['status'] in ('done','needs_recognition') else None
            sync=wait(synced)
            assert sync['status']=='done',sync
            elapsed=time.monotonic()-start
            with closing(sqlite3.connect(library/'source.sqlite')) as db:
                embeddings=db.execute('SELECT model,count(*) FROM embeddings GROUP BY model ORDER BY model').fetchall()
                labels=db.execute('SELECT count(*) FROM labels').fetchone()[0]
            assert embeddings==[('ccip',3),('wd',3)],embeddings
            assert labels>0,labels
            def researched():
                state=request('/api/similar')['state']
                if state['status']=='error':raise AssertionError(state)
                return state if state['status']=='idle' else None
            request('/api/similar',{})
            wait(researched)
            # Native imports of real hashes through the installed adapter.
            request('/api/provenance',{'action':'start'})
            provenance=wait(lambda:(v if (v:=request('/api/provenance'))['state']['status'] not in ('running','stopping') else None))
            assert provenance['state']['status'] in ('done','stopped'),provenance
            with closing(sqlite3.connect(library/'provenance.sqlite')) as db:
                hashes=db.execute('SELECT backend,count(*) FROM lookups GROUP BY backend').fetchall()
            manifest=json.loads((artifact/'manifest.json').read_text())
            for name,digest in manifest['files'].items():
                assert hashlib.sha256((artifact/name).read_bytes()).hexdigest()==digest,name
            result=dict(sync=sync, recognition_seconds=round(elapsed,3), embeddings=embeddings,
                        label_rows=labels, lookups=hashes,
                        provenance_status=provenance['state']['status'], manifest_files=len(manifest['files']))
        finally:
            host.send_signal(signal.CTRL_BREAK_EVENT);host.wait(timeout=20)
        assert host.returncode==0,host.returncode
    with socket.socket() as sock:assert sock.connect_ex(('127.0.0.1',port))!=0,'residual port'
    (work/'result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps(result,ensure_ascii=False))


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact',type=Path,required=True);parser.add_argument('--work',type=Path,required=True)
    args=parser.parse_args();run(args.artifact,args.work)

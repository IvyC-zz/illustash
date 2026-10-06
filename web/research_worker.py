#!/usr/bin/env python3
"""Optional one-shot research calculator; only the provided disposable snapshot is writable.

Launch through an explicitly configured executable accepting request.json response.json.
The stable application owns scheduling, cancellation, validation and publication.
"""
import json
import os
from pathlib import Path
import sys

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


def run(request):
    from web import learn, relate
    from web.features import Features
    from web.similar import SimilarScanner
    from web.research_snapshot import Snapshot
    snapshot = Path(request['snapshot'])
    # Snapshot source paths cannot redirect the calculator back into the application DB.
    store = Snapshot(snapshot)
    features = Features(store.source, base_dir=request['features']).load()
    os.environ['ILLUSTASH_TERMS_BIN'] = request['core']
    kind, args = request['kind'], request['args']
    out = dict(format='illustash-research/1', identity=request['identity'], version=request['version'])
    tables = []
    def progress(status, **values):
        p = os.environ.get('ILLUSTASH_RESEARCH_PROGRESS')
        if p:
            target = Path(p); temporary = target.with_suffix('.tmp')
            temporary.write_text(json.dumps(dict(status=status, **values)))
            temporary.replace(target)
    if kind == 'oc':
        out['result'] = learn.oc_groups(store, features, args['entity'])
        return out
    if kind == 'relate':
        from PIL import Image, UnidentifiedImageError
        images, missing = {}, []
        for ident in dict.fromkeys(args['ids']):
            path = store.image_path(ident)
            try:
                if not path: raise OSError('missing')
                images[ident] = relate.load(path)
            except (OSError, UnidentifiedImageError, Image.DecompressionBombError):
                missing.append(ident)
        if len(images) < 2: raise ValueError('能读到的图不足两张：原图可能还在 iCloud')
        out['result'] = relate.relate_all(images) | dict(missing=missing)
        return out
    result = {}
    if kind != 'similar':
        class Publisher:
            def snapshot(self): return request['learning_token']
            def publish(self, batch):
                out['learning'] = batch
                # Publish only to the disposable snapshot so subsequent clustering sees the same ownership.
                return store.publish_learning(request['core'], batch)
        progress('learning')
        # Patched anchors, if supplied, are local research resources, never an implicit global review directory.
        import web.library
        original_shared = web.library.SHARED
        web.library.SHARED = Path(request['anchors'])
        try:
            # shared_dir uses the snapshot directory for bare stores; supply the package feature directory explicitly.
            original = learn.shared_dir
            learn.shared_dir = lambda _: Path(request['anchors'])
            try: anchors = learn.patch_anchors(store)
            finally: learn.shared_dir = original
        finally: web.library.SHARED = original_shared
        result = learn.learn(store, features, reclaim=request['regroup'], anchors=anchors, publisher=Publisher())
        if request['regroup']:
            progress('grouping')
            result['clusters'] = learn.cluster(store, features)
            tables += ['clusters', 'cluster_info']
    if kind == 'similar' or kind == 'events' and request['regroup'] and args.get('similar'):
        class Scanner(SimilarScanner):
            def __setattr__(self, name, value):
                super().__setattr__(name, value)
                if name == 'state': progress(**value)
        scanner = Scanner(store, features)
        scanner.scan()
        result.update(scanner.state)
        tables += ['image_sigs', 'relations', 'similar_paired', 'similar_retry']
    out['tables'] = {}
    with store.connect() as c:
        for table in tables:
            out['tables'][table] = [[v.hex() if isinstance(v, bytes) else v for v in r]
                                    for r in c.execute('SELECT * FROM ' + table)]
    out['result'] = result
    return out


def main():
    request = json.loads(Path(sys.argv[1]).read_text())
    if request['format'] != 'illustash-research/1': raise ValueError('unsupported research protocol')
    response = run(request)
    out = Path(sys.argv[2]); temporary = out.with_suffix('.tmp')
    temporary.write_text(json.dumps(response, ensure_ascii=False, allow_nan=False))
    temporary.replace(out)


if __name__ == '__main__':
    main()

"""Optional WD/CCIP producer. Writes result packages only; Rust owns validation/import."""
import hashlib
import json
from pathlib import Path
import sys

if __package__ in (None, ''):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


def digest(path):
    with Path(path).open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def run(request):
    import numpy as np
    from PIL import Image, UnidentifiedImageError
    from web import infer
    if request['format'] != 'illustash-sync/1':
        raise ValueError('unsupported recognition request')
    output = Path(request['output'])
    models = dict(
        wd=dict(version='swinv2_448', dimension=1024, sha256=digest(infer.WD_MODEL), tags_sha256=digest(infer.TAGS)),
        ccip=dict(version='ccip-model_feat', dimension=768, sha256=digest(infer.ccip.MODEL_DIR + '/' + infer.ccip.FEAT_ONNX)))
    # Fresh installs have no private character anchors. Still compute all WD tags
    # and CCIP vectors; an explicitly installed anchor pair must be complete.
    anchored = infer.ANCHORS.exists() or infer.ANCHOR_IDS.exists()
    recognizer = infer.Recogniser(Path(request['identity']['package']) / 'source.sqlite', with_anchors=anchored)
    packages, failed = [], []
    assets = request['assets']
    for start in range(0, len(assets), infer.BATCH):
        ids, labels, vectors = [], [], {'wd': [], 'ccip': []}
        for asset in assets[start:start + infer.BATCH]:
            try:
                with Image.open(asset['path']) as image:
                    rows, features = recognizer.labels(image)
            except (OSError, UnidentifiedImageError):
                failed.append(dict(uuid=asset['id']))
                continue
            ids.append(asset['id'])
            labels.extend(dict(asset=asset['id'], dim=row[0], value=row[1], confidence=row[2]) for row in rows)
            for model in vectors:
                vectors[model].append(features[model])
        if not ids:
            continue
        package = output / ('batch-' + str(start))
        package.mkdir()
        (package / 'labels.jsonl').write_text('\n'.join(json.dumps(row, ensure_ascii=False) for row in labels), encoding='utf-8')
        for model in vectors:
            np.save(package / (model + '.npy'), np.asarray(vectors[model], dtype='<f4'))
        names = ('labels.jsonl', 'wd.npy', 'ccip.npy')
        manifest = dict(format='illustash-recognition/1', library_id=request['identity']['library_id'],
                        assets=ids, models=models,
                        files={name: dict(bytes=(package/name).stat().st_size, sha256=digest(package/name)) for name in names})
        (package / 'manifest.json').write_text(json.dumps(manifest), encoding='utf-8')
        packages.append(str(package))
    return dict(identity=request['identity'], packages=packages, failed=failed)


if __name__ == '__main__':
    request, response = map(Path, sys.argv[1:])
    response.write_text(json.dumps(run(json.loads(request.read_text(encoding='utf-8')))), encoding='utf-8')

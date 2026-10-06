"""Result producer contracts use generated images/vectors, never private anchors."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import numpy as np
from PIL import Image
from web import recognition_worker


class RecognitionWorkerTest(unittest.TestCase):
    def test_batches_decode_failure_fingerprints_and_no_formal_db_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); output = root / 'out'; output.mkdir()
            source = root/'source.sqlite'; source.write_bytes(b'formal sentinel')
            model = root/'wd.onnx'; model.write_bytes(b'WD')
            ccip = root/'ccip';ccip.mkdir();(ccip/'model_feat.onnx').write_bytes(b'CCIP')
            tags = root/'tags.csv';tags.write_bytes(b'tags')
            image=root/'image.png';Image.new('RGB',(20,30),'red').save(image)
            bad=root/'bad.png';bad.write_bytes(b'invalid')
            assets=[dict(id='generated-'+str(i),path=str(image)) for i in range(26)]+[dict(id='bad',path=str(bad))]
            from web import infer
            with patch.object(infer,'WD_MODEL',model), patch.object(infer,'TAGS',tags), \
                 patch.object(infer.ccip,'MODEL_DIR',str(ccip)), \
                 patch.object(infer,'ANCHORS',root/'no-anchors.npy'), patch.object(infer,'ANCHOR_IDS',root/'no-anchors.txt'), \
                 patch.object(infer,'Recogniser') as recognizer:
                recognizer.return_value.labels.return_value = ([('rating','general',.9)], {'wd':np.ones(1024,np.float32),'ccip':np.ones(768,np.float32)})
                identity=dict(package=str(root),library_id='synthetic',generation=1)
                result=recognition_worker.run(dict(format='illustash-sync/1',identity=identity,assets=assets,output=str(output)))
                recognizer.assert_called_once_with(source, with_anchors=False)
            self.assertEqual(result['identity'],identity)
            self.assertEqual(result['failed'],[dict(uuid='bad')])
            self.assertEqual(len(result['packages']),2)
            for index,name in enumerate(result['packages']):
                package=Path(name); manifest=json.loads((package/'manifest.json').read_text())
                count=25 if index==0 else 1
                self.assertEqual(len(manifest['assets']),count)
                self.assertEqual(np.load(package/'wd.npy').shape,(count,1024))
                self.assertEqual(np.load(package/'ccip.npy').shape,(count,768))
                self.assertEqual(len((package/'labels.jsonl').read_text().splitlines()),count)
                for name, record in manifest['files'].items():
                    self.assertEqual(record['sha256'],hashlib.sha256((package/name).read_bytes()).hexdigest())
                self.assertEqual(manifest['models']['wd']['sha256'],hashlib.sha256(b'WD').hexdigest())
            self.assertEqual(source.read_bytes(),b'formal sentinel')

    def test_unknown_protocol_rejected_before_output(self):
        with self.assertRaisesRegex(ValueError,'unsupported'):
            recognition_worker.run(dict(format='unexpected'))

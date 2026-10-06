"""Fresh reference-index and model-failure boundaries on generated data."""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import numpy as np
from PIL import Image
from tools import index_worker
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'tools'))
import char_patch
import ccip


class IndexWorkerTest(unittest.TestCase):
    def test_no_prior_anchors_has_a_valid_empty_feature_matrix(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(char_patch,'IDX',tmp):
            ids,matrix=char_patch.existing_anchors({'first'})
            self.assertEqual(ids,[])
            self.assertEqual(matrix.shape,(0,768))
            self.assertEqual(matrix.dtype,np.float32)

    def test_model_failure_is_not_misreported_as_bad_references(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'char-index').mkdir();(root/'char-patch').mkdir()
            image=root/'generated.png';Image.new('RGB',(10,10),'white').save(image)
            (root/'char-patch/first-refs.json').write_text(json.dumps([dict(path=str(image),id=1,general='solo 1girl')]))
            request=root/'request.json';response=root/'response.json'
            request.write_text(json.dumps(dict(format='illustash-index/1',identity={'task_id':'synthetic'},review=str(root),action='apply',rows=[dict(short_tag='first')])))
            import build_characters, char_scan
            with patch.dict(char_patch.__dict__), patch.dict(build_characters.__dict__), patch.dict(char_scan.__dict__), \
                 patch.object(ccip,'features',side_effect=RuntimeError('model unavailable')):
                with self.assertRaisesRegex(RuntimeError,'model unavailable'):
                    index_worker.main(str(request),str(response))
            self.assertFalse(response.exists())
            self.assertFalse((root/'char-patch/rejected/first.json').exists())

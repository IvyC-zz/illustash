"""旧图像参考的只读答案；完整像素位用 SHA-256 校验，无法生成或刷新夹具。"""
import base64
import gzip
import hashlib
import io
import json
from functools import lru_cache
from pathlib import Path
from types import SimpleNamespace

import numpy as np
from PIL import Image, UnidentifiedImageError


@lru_cache(None)
def records():
    with gzip.open(Path(__file__).parent/'fixtures/image_values.json.gz','rt',encoding='utf-8') as f:
        return json.load(f)


class Pixels:
    def __init__(self, record):
        self.dtype, self.shape, self.digest = record['array_key']
        self.shape = tuple(self.shape)
        self.record = record
    def min(self):
        return self.record['min']
    def ravel(self):
        if self.record['unique'] is None:
            raise AssertionError('该固定数组没有逐值枚举接口')
        return self.record['unique']


def encode(v):
    if isinstance(v, Pixels):
        return {'array_key':[v.dtype,list(v.shape),v.digest]}
    if isinstance(v,np.ndarray):
        return {'array_key':[v.dtype.str,list(v.shape),hashlib.sha256(v.tobytes()).hexdigest()]}
    if isinstance(v,np.generic):return encode(v.item())
    if isinstance(v,(bytes,bytearray)):return {'bytes':base64.b64encode(v).decode()}
    if isinstance(v,tuple):return {'tuple':[encode(x) for x in v]}
    if isinstance(v,set):return {'set':sorted([encode(x) for x in v],key=repr)}
    if isinstance(v,list):return [encode(x) for x in v]
    if isinstance(v,dict):return {'dict':[[encode(k),encode(x)] for k,x in sorted(v.items(),key=lambda x:repr(x[0]))]}
    if hasattr(v,'X') and hasattr(v,'uuids'):return {'features':[encode(v.X),v.uuids]}
    if isinstance(v,Image.Image):
        if getattr(v,'filename',None):return {'opened_image':hashlib.sha256(Path(v.filename).read_bytes()).hexdigest()}
        return {'image':[v.mode,list(v.size),hashlib.sha256(v.tobytes()).hexdigest()]}
    if isinstance(v,io.BytesIO):return {'stream':hashlib.sha256(v.getvalue()).hexdigest()}
    if isinstance(v,(str,Path)) and Path(v).is_file():return {'file':hashlib.sha256(Path(v).read_bytes()).hexdigest()}
    return str(v) if isinstance(v,Path) else v


def decode(v):
    if isinstance(v,list):return [decode(x) for x in v]
    if isinstance(v,dict):
        if 'array_key' in v:return Pixels(v)
        if 'array' in v:
            dtype,shape,data = v['array']
            return np.frombuffer(base64.b64decode(data),dtype=np.dtype(dtype)).reshape(shape)
        if 'bytes' in v:return base64.b64decode(v['bytes'])
        if 'tuple' in v:return tuple(decode(x) for x in v['tuple'])
        if 'set' in v:return set(decode(x) for x in v['set'])
        if 'dict' in v:return {decode(k):decode(x) for k,x in v['dict']}
    return v


def value(label,*args,**kwargs):
    key = hashlib.sha256(json.dumps(encode((args,kwargs)),sort_keys=True,ensure_ascii=False,allow_nan=False).encode()).hexdigest()
    result = records()[label][key]
    if 'error' in result:
        kind,message = result['error']
        classes = {'ValueError':ValueError,'OSError':OSError,'UnidentifiedImageError':UnidentifiedImageError,
                   'AttributeError':AttributeError,'TypeError':TypeError}
        raise classes[kind](message)
    return decode(result['value'])


def namespace(module,names):
    return SimpleNamespace(**{name:(lambda *a,_label=module+'.'+name,**kw:value(_label,*a,**kw)) for name in names})


relate = namespace('web.relate',('load','_resize','relate','locate','relate_all'))
similar = namespace('web.similar',('signature','grid_pairs','ccip_pairs'))
verify = namespace('provenance.verify',('signature','_sig','_colour'))
pixel_hash = namespace('provenance.sources.pixel_hash',('pixel_hash',))
wd_input = namespace('web.infer',('wd_input',)).wd_input
preprocess = namespace('tools.ccip',('preprocess',)).preprocess


def assert_bits(test,bits,expected):
    actual = np.asarray(bits,np.uint32).view(np.float32).reshape(expected.shape)
    if isinstance(expected,Pixels):
        test.assertEqual(actual.dtype.str,expected.dtype)
        test.assertEqual(hashlib.sha256(actual.tobytes()).hexdigest(),expected.digest)
    else:
        np.testing.assert_array_equal(actual.view(np.uint32),expected.view(np.uint32))

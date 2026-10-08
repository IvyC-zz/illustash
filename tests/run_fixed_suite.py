"""在导入层屏蔽退役参考实现，再运行完整 unittest discover。"""
import importlib.abc
import importlib.machinery
from pathlib import Path
import sys
import unittest

ROOT=Path(__file__).resolve().parents[1]
BLOCKED=(ROOT/'tests/reference',ROOT/'web')
RETIRED_TOOLS={ROOT/'tools'/name for name in ('index_worker.py','char_patch.py','build_characters.py','char_scan.py','ccip.py','validation_worksheet.py')}
ATTEMPTS=[]

def retired(origin):
    return origin in RETIRED_TOOLS or any(origin.is_relative_to(root) for root in BLOCKED)
class RetiredReferenceFinder(importlib.abc.MetaPathFinder):
    def find_spec(self,fullname,path=None,target=None):
        spec=importlib.machinery.PathFinder.find_spec(fullname,path)
        if spec and spec.origin and spec.origin not in ('built-in','frozen'):
            origin=Path(spec.origin).resolve()
            if retired(origin):
                ATTEMPTS.append(fullname)
                raise ImportError(f'已退役的参考模块不可导入：{fullname}')
        return None

if __name__=='__main__':
    for name,module in tuple(sys.modules.items()):
        origin=getattr(module,'__file__',None)
        if origin and retired(Path(origin).resolve()):
            raise RuntimeError(f'退役参考已被提前导入：{name}')
    sys.meta_path.insert(0,RetiredReferenceFinder())
    program=unittest.main(module=None,argv=[sys.argv[0],'discover','-s','tests',*sys.argv[1:]],exit=False)
    print(f'退役参考导入尝试：{len(ATTEMPTS)}')
    sys.exit(not program.result.wasSuccessful() or bool(ATTEMPTS))

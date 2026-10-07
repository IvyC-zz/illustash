"""独立 NumPy 余弦距离定义，用于数学契约，不执行业务算法。"""
import numpy as np

def cross(a,b,check=False):
    a=np.asarray(a,dtype=np.float32);b=np.asarray(b,dtype=np.float32)
    a=a/np.maximum(np.linalg.norm(a,axis=1,keepdims=True),1e-12)
    b=b/np.maximum(np.linalg.norm(b,axis=1,keepdims=True),1e-12)
    return (1-a@b.T)/2

"""CCIP 角色相似度 —— 经本机实测修正的两阶段实现。

官方流程是两步，不是一步：

    model_feat.onnx     图像 -> (768,) 特征
    model_metrics.onnx  (n, 768) -> (n, n) 差异矩阵

把两者合并成 numpy 距离是错的。用 `norm(feat_a - feat_b)` 会得到
163~408 这个量级的数字，而判定阈值是 0.178 —— 结果就是每一对图
都被判成「不同角色」，检索永远返回空，而且不报任何错。

用法：
    from tools.ccip import features, differences, threshold
    F = features(paths)                 # (n, 768)
    D = differences(F)                  # (n, n)
    same = D <= threshold()             # 布尔矩阵

实测（MacBook M2, CPU EP, 384px）：
    特征提取  216.6 ms/张
    差异矩阵    0.13 ms（6 张，即 (6,6)）—— 相对提取可忽略
"""
import json
import os

import numpy as np
import onnxruntime as ort
from PIL import Image

_HERE = os.path.dirname(os.path.abspath(__file__))
MODEL_DIR = os.path.join(os.environ.get("ILLUSTASH_MODELS", os.path.join(os.path.dirname(_HERE), "models")), "ccip")

FEAT_ONNX = "model_feat.onnx"
METRIC_ONNX = "model_metrics.onnx"

# CLIP 的均值方差，CCIP 沿用了它
MEAN = np.asarray((0.48145466, 0.4578275, 0.40821073), dtype=np.float32)
STD = np.asarray((0.26862954, 0.26130258, 0.27577711), dtype=np.float32)

_sessions = {}


def _get(filename: str) -> ort.InferenceSession:
    if filename not in _sessions:
        opts = ort.SessionOptions()
        opts.log_severity_level = 3
        _sessions[filename] = ort.InferenceSession(
            os.path.join(MODEL_DIR, filename),
            opts,
            providers=["CPUExecutionProvider"],   # CoreML 在 M2 上更慢
        )
    return _sessions[filename]


def preprocess(image, size: int = 384) -> np.ndarray:
    """直接拉伸成正方形（squash，不是 padding），CLIP 归一化，NCHW。"""
    if isinstance(image, (str, bytes, os.PathLike)):
        image = Image.open(image)
    im = image.convert("RGB").resize((size, size), Image.BILINEAR)
    d = np.asarray(im, dtype=np.float32) / 255.0
    d = (d - MEAN[None, None, :]) / STD[None, None, :]
    return np.ascontiguousarray(d.transpose(2, 0, 1)[None])


def features(images, size: int = 384) -> np.ndarray:
    """一批图 -> (n, 768) 特征矩阵。单张约 217 ms。"""
    sess = _get(FEAT_ONNX)
    name = sess.get_inputs()[0].name
    return np.stack(
        [sess.run(None, {name: preprocess(im, size)})[0][0] for im in images]
    ).astype(np.float32)


def differences(feats: np.ndarray) -> np.ndarray:
    """(n, 768) -> (n, n) 差异矩阵。必须走 model_metrics，不要自己算距离。"""
    sess = _get(METRIC_ONNX)
    out, = sess.run(["output"], {"input": np.ascontiguousarray(feats, dtype=np.float32)})
    return np.asarray(out)


def cross(a: np.ndarray, b: np.ndarray, check: bool = True) -> np.ndarray:
    """(m,768) × (n,768) -> (m,n) 差异矩阵，不跑 model_metrics。

    `model_metrics.onnx` 实测就是 **(1 - 余弦相似度) / 2**：
    400 个真实特征的 79,800 对上，pearson 和 spearman 都是 1.00000000，
    最大绝对差 7e-7（float32 精度）。外部 character_index 的特征混进来也一样。

    所以 ERRATA §1 那条要说得更准确一点：
        ❌ `norm(a - b)`          —— 错的，量级 163~408，阈值是 0.178，永远判不同
        ✅ `(1 - cos(a, b)) / 2`  —— 与 model_metrics 等价，误差 7e-7

    为什么要这个：`differences()` 只能算方阵 (n,n)，扫 7,380 个锚点 × 46,980 张图
    得切成 96 块、算一堆用不上的对角块，十几分钟且吃内存；`cross()` 是一次
    矩阵乘法，几秒钟，内存按块可控。

    `check=True` 时随机抽 ≤64×64 个位置回跑一次 `differences()` 对拍 ——
    静默失效是这个项目踩过最狠的坑，多花 0.1 秒买个保险。
    """
    a = np.ascontiguousarray(a, dtype=np.float32)
    b = np.ascontiguousarray(b, dtype=np.float32)
    ua = a / np.maximum(np.linalg.norm(a, axis=1, keepdims=True), 1e-12)
    ub = b / np.maximum(np.linalg.norm(b, axis=1, keepdims=True), 1e-12)
    d = (1.0 - ua @ ub.T) / 2.0
    if check:
        m, n = min(len(a), 48), min(len(b), 48)
        ia = np.random.default_rng(0).choice(len(a), m, replace=False)
        ib = np.random.default_rng(1).choice(len(b), n, replace=False)
        ref = differences(np.vstack([a[ia], b[ib]]))[:m, m:]
        err = float(np.abs(ref - d[np.ix_(ia, ib)]).max())
        if err > 1e-4:
            raise AssertionError(
                "cross() 与 model_metrics 对不上（最大差 %.3e）—— 不要用这个结果" % err)
    return d


def threshold() -> float:
    """从 metrics.json 读，不要硬编码 —— 各变体的阈值差别很大。"""
    with open(os.path.join(MODEL_DIR, "metrics.json")) as f:
        return json.load(f)["threshold"]


def same_character(a, b) -> bool:
    """两张图是否为同一角色。注意：官方限定单角色图，多角色图先裁人再比。"""
    return bool(differences(features([a, b]))[0, 1] <= threshold())


if __name__ == "__main__":
    import time

    d = os.path.join(os.path.dirname(_HERE), "reference", "test_images")
    paths = [os.path.join(d, f) for f in sorted(os.listdir(d))]
    t = time.time()
    F = features(paths)
    t_feat = (time.time() - t) / len(paths)
    D = differences(F)
    th = threshold()
    iu = np.triu_indices(len(paths), 1)
    print(f"{len(paths)} images | features {F.shape} @ {t_feat*1000:.1f} ms/img | diffs {D.shape}")
    print(f"threshold {th}")
    print(f"off-diagonal min {D[iu].min():.4f}  median {np.median(D[iu]):.4f}  max {D[iu].max():.4f}")
    print(f"pairs judged same character: {(D[iu] <= th).sum()} / {len(iu[0])}")
    print(f"projected for 47864 images: {47864 * t_feat / 3600:.2f} h")

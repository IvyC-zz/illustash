# 多人图的 CCIP 裁图对照

这是 #456 离线评测的复跑入口。使用冻结的公开评测数据、已发布的参考、公开模型和本地计算设备。输出目录保存图片、特征和逐角色指标。

## 输入

- [共用评测集 v1](https://github.com/shio-vinyl/illustash/pull/628) 的 `eval/manifest.json` 有 20,366 张图，SHA-256 为 `93dfec196a73102787864a0ab9f0fcefc26d44a284616a7abb3ff62f03ebba11`。分别从 #557 的两次冻结拟合取 `A-metadata.json` 和 `development-accepted.json`，从已合并的 92 个长尾锚点取 `A-metadata.json` 和 `A-anchors.f32`。7,367 角色的完整精选索引用 #534 的 `index.npz`（`987f659809a320b63c14aac94f4804133349ddf7f0639a5ed9df9f10b4280f33`）和 `loaded-characters.json`。别名字典用同一共用集的公开标签快照。
- 从 [Danbooru 镜像索引](https://huggingface.co/datasets/petersunde/danbooru2026-index) 取 `tar-index.parquet`，用 #380 的公开帖子元数据补原始尺寸。从 [Gelbooru 镜像](https://huggingface.co/datasets/nyanko-devs/gelbooru2026/tree/3dd3a561e35cca9fc2603b84c0c854b5a029cf5e) 取 `metadata/posts.parquet`。两个获取脚本固定了实际提供图片的数据集版本。
- 把模型放在输出目录的 `models/`，依次命名为 `current.onnx`、`b36.onnx`、`person.onnx`、`head.onnx`。现用 CCIP 取自应用发布的 `ccip/model_feat.onnx`；另三份分别是 [b36-24](https://huggingface.co/deepghs/ccip_onnx/tree/eb2acdd29af1703388d3d0c04221add322bc9110/ccip-caformer_b36-24)、[人物 v1.1_m](https://huggingface.co/deepghs/anime_person_detection/tree/e39c744c22432ad01f91dd254fe2b02c8d878b8c/person_detect_v1.1_m)、[头部 v2.0_s](https://huggingface.co/deepghs/anime_head_detection/tree/06604feee81983792a57c21081e539c0ae229833/head_detect_v2.0_s)。启动提取时核对四份 ONNX 的字节身份。

## 复跑

运行 Python 需 NumPy、Pillow、DuckDB、requests，以及对应设备的 ONNX Runtime。原生扫描器沿用 #534，直接调用应用的角色索引和阈值计算。编译可在独立的 Illustash 工作树把本目录的 `native_scan.rs` 放到 `native/terms/examples/crop-ccip-scan.rs`，再运行 `cargo build --locked --manifest-path native/terms/Cargo.toml --example crop-ccip-scan`。以下变量分别指向上面列出的输入，所有命令从本目录运行。

```sh
python sample.py --manifest "$MANIFEST" --references "$LT1" "$LT2" --index "$TAR_INDEX" --metadata "$POSTS" --output "$OUT"
python download.py "$OUT"
python supplement.py "$OUT" --metadata "$GEL_METADATA"
python features.py "$OUT" --device dml --target "$OUT/features-dml"
python score.py "$OUT" --features "$OUT/features-dml" --index "$CURATED_INDEX" --pack "$LONGTAIL_PACK" --native "$SCAN" --aliases "$ALIASES" --output "$OUT/score.json"
python -m unittest discover -p test_score.py
```

本轮 `sample.json` 的 SHA-256 是 `94079d6b8811d261cc32e866852e1c4ac202fa18d4de24729054efd4f9b92ff0`，`anchors.json` 是 `11a105210c7f41897573019ed4a36ba7e1c430b341b995920a461ee747d93660`。每图收据记录实际来源、尺寸和内容 SHA-256。共用数据的原帖 ID 与图像摘要用于保留可比性。原图按原帖 MD5 核对，两个 WebP 来源都是重编码文件。网络错误、索引无条目和没有检测框分别记数。

训练不读取评测反馈。每种裁法要求同一参考图在冻结标注里是单人、只有该角色，且该检测器恰好给出一框。每个角色至少五张，同组两款模型、整图与裁图的参考帖子完全一致。以等权单位向量中心和训练集距离 p75 对比同组角色。生产基线另用完整精选和已发布长尾包做最近九项、别名和阈值重放。计数单位是图和角色的组合，同一角色的多框命中合并；公开标注没有提供逐框身份。

# 第三方组件与许可

状态：现行参考，2026-10-06。本项目代码以 GPL-3.0-or-later 发布（见仓库根目录 `LICENSE`）；本文列出随代码或安装包分发的第三方组件及其许可。模型与数据不受本项目代码许可约束，按各自许可使用。Illustash 名称与图标不随代码授权。

## Rust 依赖

完整依赖及版本由 `native/terms/Cargo.lock`、`desktop/Cargo.lock` 固定。2026-10-06 清点：`native/terms` 212 个、`desktop` 508 个第三方 crate，几乎全部为 MIT、Apache-2.0、BSD、ISC、Zlib、Unicode-3.0 等宽松许可或其多选一组合；`desktop` 另有 5 个 MPL-2.0 crate（Tauri 生态，按文件级开源义务使用）。标注含 LGPL 的 crate 均为与 MIT/Apache-2.0 多选一。上述许可均与 GPL-3.0 兼容。

## 随源码分发

| 组件 | 位置 | 许可 |
|---|---|---|
| PocketFFT | `native/terms/vendor/pocketfft/` | BSD-3-Clause，原文 `native/terms/vendor/pocketfft/LICENSE.md` |
| LittleCMS 2.19 及 lcms2 封装层 | Rust 依赖 | MIT，原文 `native/terms/licenses/LCMS.txt` |
| Unicode 字符数据 | `crates/core` 的 casefold 与 HTTP 字符表，由 `tools/gen_casefold.py`、`tools/gen_http_unicode.py` 从 Python `unicodedata`（Unicode 15.0）生成 | [Unicode License v3](https://www.unicode.org/license.txt) |
| 标签表 | `tags/tags_v3.csv`、`tags/tags_v14.csv` | 取自 WD tagger 模型仓库的 `selected_tags.csv`（Apache-2.0）；标签名与计数源自 [Danbooru](https://danbooru.donmai.us) |

PocketFFT 与 LittleCMS 的许可原文随基础与计算资源安装到 `resources/licenses/`，见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)。

## 打包时加入的运行库

构建脚本不下载任何文件，以下组件由打包者显式提供，许可原文按原字节放入安装包：

| 组件 | 平台 | 许可 |
|---|---|---|
| ONNX Runtime（CPU） | macOS、Windows | MIT；随包附 `LICENSE` 与 `ThirdPartyNotices.txt` |
| OpenBLAS（取自固定版本 NumPy wheel） | Windows | BSD-3-Clause；随包附 wheel 的完整许可目录 |

## 模型与数据

仓库不含模型。完整安装包按白名单加入以下模型；分发带模型的安装包须同时遵守各自许可，CCIP 的 OpenRAIL 许可附带使用限制，发布前须随包附上原文。

| 名称 | 来源 | 用途 | 许可 |
|---|---|---|---|
| WD SwinV2 Tagger v3 | [SmilingWolf/wd-swinv2-tagger-v3](https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3) | 图片标签与角色识别 | Apache-2.0 |
| CCIP | [deepghs/ccip](https://huggingface.co/deepghs/ccip)、[deepghs/ccip_onnx](https://huggingface.co/deepghs/ccip_onnx) | 角色特征比对 | OpenRAIL |
| Character Index | [deepghs/character_index](https://huggingface.co/datasets/deepghs/character_index) | 角色索引数据（运行时按需下载） | MIT |

应用「关于」页另列出致谢的上游项目与数据源（Danbooru、Bangumi 等）；联网获取的数据按各站点条款使用，见 [找回画师](PROVENANCE.md) 与 [界面名称](WEB_NAMES.md)。

# 第三方组件与许可

状态：现行参考，2026-10-16。本项目代码以 GPL-3.0-or-later 发布（见仓库根目录 `LICENSE`）；本文列出随代码或安装包分发的第三方组件及其许可。模型与数据不受本项目代码许可约束，按各自许可使用。Illustash 名称与图标不随代码授权。

## Rust 依赖

完整依赖及版本由 `native/terms/Cargo.lock`、`desktop/Cargo.lock` 固定。2026-10-06 清点：`native/terms` 212 个、`desktop` 508 个第三方 crate，几乎全部为 MIT、Apache-2.0、BSD、ISC、Zlib、Unicode-3.0 等宽松许可或其多选一组合；`desktop` 另有 5 个 MPL-2.0 crate（Tauri 生态，按文件级开源义务使用）。标注含 LGPL 的 crate 均为与 MIT/Apache-2.0 多选一。上述许可均与 GPL-3.0 兼容。

## 随源码分发

| 组件 | 位置 | 许可 |
|---|---|---|
| PocketFFT | `native/terms/vendor/pocketfft/` | BSD-3-Clause，原文 `native/terms/vendor/pocketfft/LICENSE.md` |
| LittleCMS 2.19 及 lcms2 封装层 | Rust 依赖 | MIT，原文 `native/terms/licenses/LCMS.txt` |
| Unicode 字符数据 | `crates/core` 的 casefold 与 HTTP 字符表，由 `tools/gen_casefold.py`、`tools/gen_http_unicode.py` 从 Python `unicodedata`（Unicode 15.0）生成 | [Unicode License v3](https://www.unicode.org/license.txt) |
| 标签表 | `tags/tags_v3.csv`、`tags/tags_v14.csv` | 取自 WD tagger 模型仓库的 `selected_tags.csv`（Apache-2.0）；标签名与计数源自 [Danbooru](https://danbooru.donmai.us) |

每个分发产物（便携包、基础与完整安装包）的 `resources/licenses/` 里有本项目 `LICENSE`、本文（`THIRD_PARTY.md`）以及 PocketFFT、LittleCMS 的许可原文，见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)。Rust 依赖目前只随包附 `Cargo.lock` 版本清单，各 crate 的许可正文不随包附带（#8）。

## 打包时加入的运行库

构建脚本不下载任何文件，以下组件由打包者显式提供，许可原文按原字节放入安装包：

| 组件 | 平台 | 许可 |
|---|---|---|
| ONNX Runtime（CPU） | macOS、Windows | MIT；随包附 `LICENSE` 与 `ThirdPartyNotices.txt` |
| OpenBLAS（取自固定版本 NumPy wheel） | Windows | BSD-3-Clause；随包附 wheel 的完整许可目录 |

## 模型与数据

仓库不含模型。完整安装包按白名单加入以下模型，分发时须同时遵守各自的许可。

| 名称 | 随包文件的来源 | 用途 | 许可 |
|---|---|---|---|
| WD SwinV2 Tagger v3 | [deepghs/wd14_tagger_with_embeddings](https://huggingface.co/deepghs/wd14_tagger_with_embeddings) 的 `SmilingWolf/wd-swinv2-tagger-v3/model.onnx`（DeepGHS 加了向量输出的重导出），原模型 [SmilingWolf/wd-swinv2-tagger-v3](https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3) | 图片标签与角色识别 | Apache-2.0 |
| CCIP | [deepghs/ccip_onnx](https://huggingface.co/deepghs/ccip_onnx) 的 `ccip-caformer-24-randaug-pruned/`，原模型 [deepghs/ccip](https://huggingface.co/deepghs/ccip) | 角色特征比对 | 上游声明 OpenRAIL；随包按 BigScience Open RAIL-M 分发 |

**随包的许可原文**（2026-10-16，#8）：原文提交在 `packaging/model-licenses/`，是打包的唯一来源。`sources.json` 登记每个文件的上游地址和 SHA-256。只要打包时带了模型（`--models`），就把这个目录整份按原字节放进 `resources/licenses/models/`；文件缺失、多出、内容与登记不符，打包都会报错，不会产出半成品。Windows 和 macOS 完整安装包的暂存步骤也把这些文件列为必需组件。内部测试包只要带模型，也一样附上。

- WD：Apache-2.0 原文（apache.org 的 `LICENSE-2.0.txt`），加上两个上游仓库在固定 revision 下的模型卡。两个仓库都没有 NOTICE 文件。
- CCIP：两个上游模型卡只写了 `license: openrail`。Hugging Face 对这个标识的定义是「OpenRAIL 许可家族」，没有对应的条款文本。2026-10-16 用户决定：按最通用的 BigScience Open RAIL-M（2022-08-18 版，BigScience 发布页链接的 PDF 原件）随包，同时附两份模型卡。该许可第 5 条及附件 A 的使用限制对使用者生效，再分发时必须一并传递。同日查过几个使用 CCIP 的开源项目：多数写「OpenRAIL，以模型卡为准」，不附条款原文，且运行时才下载、不随包；没有一家附了确定的条款文本。
- `NOTICE.txt`（英文）列出每个随包模型文件的上游仓库、revision、SHA-256 和对应的许可文件，并写明 CCIP 的使用限制。
- 更新上游许可或模型时，替换原文、改 `sources.json` 的哈希，并同步 `NOTICE.txt`。
| Character Index | [deepghs/character_index](https://huggingface.co/datasets/deepghs/character_index) | 已有共享角色索引的来源数据；不随安装包分发，应用也不下载。新用户补充角色参考特征走「检查新角色」（Danbooru 公开图片，每次运行前确认） | MIT |

应用「关于」页另列出致谢的上游项目与数据源（Danbooru、Bangumi 等）；联网获取的数据按各站点条款使用，见 [找回画师](PROVENANCE.md) 与 [界面名称](WEB_NAMES.md)。

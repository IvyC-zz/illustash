# 第三方组件与许可

状态：现行参考，2026-10-19。本项目代码以 GPL-3.0-or-later 发布（见仓库根目录 `LICENSE`）；本文列出随代码或安装包分发的第三方组件、应用第一次使用时下载的模型，以及它们的许可。模型与数据不受本项目代码许可约束，按各自许可使用。Illustash 名称与图标不随代码授权。

## Rust 依赖

完整依赖及版本由 `native/terms/Cargo.lock`、`desktop/Cargo.lock`、`crates/iqdb/Cargo.lock` 固定。2026-10-16 清点：三个 workspace 共 518 个第三方 crate（含 vendored 的 `tiny_http`），几乎全部为 MIT、Apache-2.0、BSD、ISC、Zlib、Unicode-3.0 等宽松许可或其多选一组合；`desktop` 另有 5 个 MPL-2.0 crate（Tauri 生态，按文件级开源义务使用）。标注含 LGPL 的 crate 均为与 MIT/Apache-2.0 多选一，按后者使用。上述许可均与 GPL-3.0 兼容。

**随包的许可正文**（2026-10-16，#161）：`packaging/rust-licenses/rust-dependencies.txt` 收录每个第三方 crate 的许可正文，随每个分发产物放进 `resources/licenses/rust-dependencies.txt`，并在 manifest 里登记。文件开头按 crate 列出名称、版本、声明的许可表达式和适用正文的编号，后面是去重后的正文；多选一的 crate 只收录选用的那一份，优先顺序见 `about.toml` 的 `accepted`。正文取自各 crate 发布包里的许可文件（含子目录里嵌入的上游代码许可，如 mozjpeg-sys 带的 libjpeg-turbo）；没带许可文件的 crate 用 SPDX 标准文本。

- 生成：`tools/gen_rust_licenses.py` 调用固定版本的 [cargo-about](https://github.com/EmbarkStudios/cargo-about) 0.9.2（MIT OR Apache-2.0），离线读本机 cargo 注册表里的 crate 源码，按全部 feature、全部目标平台解析三个 workspace，构建期和测试用 crate 也收录。结果和 `crates.json` 一起提交；`crates.json` 登记覆盖的 crate、正文的 SHA-256，以及锁文件里列出但任何 feature 组合都不会编译的可选依赖（`not_built`，不附正文）。打包脚本和检查都不需要 cargo-about，也不下载任何东西。
- 检查：打包前和 `tests/test_packaging_resources.py` 都比对 `crates.json` 与三份 `Cargo.lock`。附了正文的 crate 加上 `not_built` 必须恰好等于锁文件里的全部第三方包（按名称加版本），正文哈希必须与登记一致；不一致时测试失败、打包在产出任何文件之前拒绝执行。我们自己的 crate（`illustash-*`，GPL-3.0-or-later）不列入。
- 更新：改了任何 `Cargo.lock` 或依赖的 feature 之后都要重新生成。只改 feature、没改锁文件却启用了某个 `not_built` 包时，上面的检查发现不了，必须手动重跑。新依赖用了 `accepted` 以外的许可时，生成会失败，先确认与 GPL-3.0 兼容再加进 `about.toml`。

```bash
cargo install --locked --features cli cargo-about@0.9.2
```

```bash
python3 tools/gen_rust_licenses.py
```

生成要求三个 workspace 的全部 crate 源码已在本机 cargo 注册表里（包括其他平台才用到的 crate）；缺的话先对每个 workspace 运行 `cargo fetch --locked --manifest-path <workspace>/Cargo.toml`。

## 随源码分发

| 组件 | 位置 | 许可 |
|---|---|---|
| PocketFFT | `native/terms/vendor/pocketfft/` | BSD-3-Clause，原文 `native/terms/vendor/pocketfft/LICENSE.md` |
| LittleCMS 2.19 及 lcms2 封装层 | Rust 依赖 | MIT，原文 `native/terms/licenses/LCMS.txt` |
| Unicode 字符数据 | `crates/core` 的 casefold 与 HTTP 字符表，由 `tools/gen_casefold.py`、`tools/gen_http_unicode.py` 从 Python `unicodedata`（Unicode 15.0）生成 | [Unicode License v3](https://www.unicode.org/license.txt) |
| 标签表 | `tags/tags_v3.csv`、`tags/tags_v14.csv` | 取自 WD tagger 模型仓库的 `selected_tags.csv`（Apache-2.0）；标签名与计数源自 [Danbooru](https://danbooru.donmai.us) |

每个分发产物（便携包、安装包）的 `resources/licenses/` 里有本项目 `LICENSE`、本文（`THIRD_PARTY.md`）、PocketFFT 和 LittleCMS 的许可原文，以及 Rust 依赖的许可正文 `rust-dependencies.txt`（见上节），见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)。

## 打包时加入的运行库

构建脚本不下载任何文件，以下组件由打包者显式提供，许可原文按原字节放入安装包：

| 组件 | 平台 | 许可 |
|---|---|---|
| ONNX Runtime（CPU） | macOS、Windows | MIT；随包附 `LICENSE` 与 `ThirdPartyNotices.txt` |
| OpenBLAS（取自固定版本 NumPy wheel） | Windows | BSD-3-Clause；随包附 wheel 的完整许可目录 |

## 模型与数据

仓库不含模型。2026-10-16 用户决定（#168）：首个内测版起发布的安装包也不带模型，应用第一次使用时从上游下载；按白名单带上模型的完整安装包只用于开发和测试。下载与随包用的是同一组文件，见下表；逐文件的大小、SHA-256 和下载地址写在应用内置的清单 `native/terms/models.json` 里，使用者手动下载的说明见 [装不上、打不开怎么办](INSTALL.md#识别模型)。

| 名称 | 文件的来源 | 用途 | 许可 |
|---|---|---|---|
| WD SwinV2 Tagger v3 | [deepghs/wd14_tagger_with_embeddings](https://huggingface.co/deepghs/wd14_tagger_with_embeddings) 的 `SmilingWolf/wd-swinv2-tagger-v3/model.onnx`（DeepGHS 加了向量输出的重导出），原模型 [SmilingWolf/wd-swinv2-tagger-v3](https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3) | 图片标签与角色识别 | Apache-2.0 |
| CCIP | [deepghs/ccip_onnx](https://huggingface.co/deepghs/ccip_onnx) 的 `ccip-caformer-24-randaug-pruned/`，原模型 [deepghs/ccip](https://huggingface.co/deepghs/ccip) | 角色特征比对 | 上游声明 OpenRAIL；随包按 BigScience Open RAIL-M 分发 |

**下载来源**（2026-10-19，#168）：应用依次尝试两个来源：Hugging Face 上游仓库固定提交（WD `4ec31f2`、CCIP `eb2acdd`）的地址，经可配置的镜像（默认 hf-mirror.com）；ModelScope 上同名仓库 `deepghs/wd14_tagger_with_embeddings`、`deepghs/ccip_onnx` 的副本。ModelScope 的两个仓库不是 DeepGHS 官方维护的（平台账号搬运，最后更新于 2024-12 / 2025-02），2026-10-16 核对时文件与上游逐字节相同。应用只认清单里的 SHA-256，所以来源不影响拿到的文件。用户下载模型是从上游取得，不是本项目再分发；许可原文仍随每个计算包附上（见下一段），关于页照常写明 CCIP 的使用限制。

**随包的许可原文**（2026-10-16，#8；2026-10-19 起不论带不带模型，#168）：原文提交在 `packaging/model-licenses/`，是打包的唯一来源。`sources.json` 登记每个文件的上游地址和 SHA-256。每个计算包（`--research`）都把这个目录整份按原字节放进 `resources/licenses/models/`；文件缺失、多出、内容与登记不符，打包都会报错，不会产出半成品。Windows 和 macOS 安装包的暂存步骤也把这些文件列为必需组件。

- WD：Apache-2.0 原文（apache.org 的 `LICENSE-2.0.txt`），加上两个上游仓库在固定 revision 下的模型卡。两个仓库都没有 NOTICE 文件。
- CCIP：两个上游模型卡只写了 `license: openrail`。Hugging Face 对这个标识的定义是「OpenRAIL 许可家族」，没有对应的条款文本。2026-10-16 用户决定：按最通用的 BigScience Open RAIL-M（2022-08-18 版，BigScience 发布页链接的 PDF 原件）随包，同时附两份模型卡。该许可第 5 条及附件 A 的使用限制对使用者生效，再分发时必须一并传递。同日查过几个使用 CCIP 的开源项目：多数写「OpenRAIL，以模型卡为准」，不附条款原文，且运行时才下载、不随包；没有一家附了确定的条款文本。
- `NOTICE.txt`（英文）列出每个模型文件的上游仓库、revision、SHA-256 和对应的许可文件。CCIP 一节就是该模型的分发条款：写明随包的 CCIP 按 Open RAIL-M 许可给使用者，附件 A 的使用限制是条款中可执行的部分，再分发者须把限制写进自己的分发协议、告知接收者并附上许可。这满足该许可第 4(a)(b) 条。
- 2026-10-16 用户决定（#163）：不另设需要点击接受的协议，安装向导和首次启动都不加确认；应用「关于」页的「致谢与第三方组件」和 README「许可」一节写明 CCIP 受 Open RAIL-M 第 5 条约束及原文位置。不向上游 DeepGHS 征询具体条款文本；上游日后指明别的版本时，替换原文并改 `NOTICE.txt` 的版本说明。
- 更新上游许可或模型时，替换原文、改 `sources.json` 的哈希，并同步 `NOTICE.txt`。
| Character Index | [deepghs/character_index](https://huggingface.co/datasets/deepghs/character_index) | 已有共享角色索引的来源数据；不随安装包分发，应用也不下载。新用户补充角色参考特征走「检查新角色」（Danbooru 公开图片，每次运行前确认） | MIT |

应用「关于」页另列出致谢的上游项目与数据源（Danbooru、Bangumi 等）；联网获取的数据按各站点条款使用，见 [找回画师](PROVENANCE.md) 与 [界面名称](WEB_NAMES.md)。

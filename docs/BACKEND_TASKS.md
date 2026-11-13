# 后端任务交付边界

状态：现行参考，2026-11-12；任务职责与 [架构方向](ARCHITECTURE_DIRECTION.md) 配套，安装交付以本篇为准。识别执行后端统一见本篇「识别的执行后端」；溯源的联网与真实数据验收边界见「验收」。本地 OCR 计算已删除，历史结果不作为现行计算能力。

## 当前交付状态（2026-09-28）

已实现：溯源任务互斥、取消、超时、异常恢复与增量补跑；Windows 识别结果包的事务导入、去重与恢复；显式初始化/查询入口及事务后业务事件；Pixiv 关注和昵称 worker。本地 OCR 计算已于 2026-10-05 按用户决定删除（#177），已有 OCR 结果与人工判断保留。结果包命令、HTTP 接口及事务边界见 [RESULT_PACKAGES.md](RESULT_PACKAGES.md)。

2026-09-28 的验收覆盖 macOS 离线测试与真实图库副本；2026-10-01 的原生默认入口和安装包集成验收仅使用合成数据。Windows x64 的基础功能已在 2026-10-01 实机验收（见下文「Windows」），Windows 原生识别生产端已做真实模型与生成图片的合成验收（见 [结果包](RESULT_PACKAGES.md)）；这不等于真实图片质量、大图库或用户从安装到日常使用的完整链路验收。Pixiv 真实账号响应与后续画师归属的人工准确率验收仍未完成。

## 溯源生命周期

溯源任务由原生 Runner（`native/terms/src/app_provenance.rs`）管理：租约、互斥、取消、恢复和补跑，以及按「多久没有进展」判超时（默认 600 秒），现行规则见 [PROVENANCE.md](PROVENANCE.md) 第 8 节。进度除文字外还有 worker 报的结构化数字（状态的 `progress`：`label`、`done`、`total`，可选 `found`、`mb` 和第二层的 `sub_label`、`sub_done`、`sub_total`），任务面板画成进度条和流量条（`web/static/ui/tasks.js` 的 `provenanceBars`）。

增量范围：pixel_hash 对有效 x/pixiv 资产中尚无 hit/miss 的项执行。因此涵盖以往失败或遗漏的积压，不限于本次同步的新 ID。verify 的缓存带当前应用资产集合指纹；新增/移除资产后重新比对已有候选画师，逐图匹配，不把一个图的判断传播给其他图。

外部请求默认不授权。HTTP 启动单次任务增加 `allow_network:true`；自动任务需要启动服务时显式 `--allow-provenance-network`。授权参数不写入默认设置，不因保存账号凭据而自动授权。未授权步骤记录 stopped。任务面板的「开始…」先展开本次会发出什么（发给谁、下载什么），用户点「授权并开始」才附带 `allow_network:true`，也可以选「只跑本地」；授权只对这一次有效，不保存。

## 来源

- `pixiv_following`：分页、可续跑的公开关注列表，只产生 seed；跟随关系不能证明画师归属。
- `nicknames`：按稳定 Pixiv 用户 ID 更新独立 artist_names，重名不会合并画师。

新增来源共用 evidence 的候选格式、响应缓存与重试元数据。缓存有效期 7 天；错误 1 小时后可重试，miss 1 天、hit 7 天。403/429 等拒绝仍由 Net 停止该主机，不绕过风控。已有 pixel_hash / verify 的专用缓存保留原语义。

## 验收

Pixiv 登录过期：运行前检查绑定的登录，带登录收到 401 也按过期处理，本次停掉 Pixiv 而不退回未登录查询，设置页提示重新绑定（见 PROVENANCE.md 第 8 节）。2026-09-28 真实账号联测被 Cloudflare 人机验证（403）拦下，未绕过。后续改由 app 内嵌 Pixiv 登录窗口发请求（PROVENANCE.md 第 9 节）。

尚未验证：Pixiv 实际账号响应、真实图片识别质量及大图库。Windows 研究 worker 与真实模型的合成验收见下文，不计为真实账号/图片验收。

## 独立安装与可选计算包

共享资源、图库和研究输出的职责及相对路径兼容规则以 [图库参考](LIBRARIES.md#2-包外的共享资源) 为准。物理整理不改变本节的安装白名单；共享数据由 `ILLUSTASH_SHARED` 一处配置，安装计算进程沿用解析后的绝对路径。

基础版生产资源包含 `bin/illustash-terms`（Windows 为 `.exe`）、`resources/web/static/`、`resources/tags/tags_v3.csv` 及 `resources/licenses/`（本项目 `LICENSE`、`THIRD_PARTY.md`、第三方许可原文与 Rust 依赖许可正文 `rust-dependencies.txt`）。图片来自显式打开的图库，图库、账号、缓存、模型、`.venv`、开发 `target/` 均不打进包。SQLite 随 Rust 构建；完整 Rust 依赖及版本分别由 `native/terms/Cargo.lock`、`desktop/Cargo.lock`、`crates/iqdb/Cargo.lock` 固定，各 crate 的许可正文由仓库里提交的 `packaging/rust-licenses/` 提供；正文与锁文件不一致时打包拒绝执行，更新办法见 [第三方组件与许可](THIRD_PARTY.md#rust-依赖)。桌面另需系统 WebView（macOS WKWebView；Windows WebView2）及操作系统图像适配组件。Python 是打包脚本的构建工具，不是基础程序运行依赖。首次建库使用程序内置的空 source schema，不需要 `review/` 或 `ILLUSTASH_SHARED`；命令、参考库校验和「待识别」状态见 [LIBRARIES.md](LIBRARIES.md#首次建库与参考库)。

在仓库根目录运行 `python3 packaging/build.py --output /tmp/illustash-base`，脚本执行 `cargo build --locked --release --manifest-path native/terms/Cargo.toml --bins`，按白名单复制资源，并产生逐文件 SHA-256 的 `manifest.json` 和 `dependencies/core-Cargo.lock` 生产依赖清单。加 `--archive /tmp/illustash-base.zip` 可生成按路径排序、固定 ZIP 时间戳并保留 Unix 权限的发布归档；已有对应平台构建可用 `--binaries PATH`，调试构建可用 `--profile dev`；产物、归档和桌面暂存目录必须不存在，避免把旧文件带入新安装包。将输出整体复制到安装位置后，运行 `./start.sh --library /absolute/synthetic.pilib --no-sync --no-open`（Windows：`start.cmd ...`）。也可直接运行 `bin/illustash-terms http --native --library ...`，原生入口从可执行文件相邻的 `resources/` 定位资源；显式 `--root` 和 `ILLUSTASH_ROOT` 可覆盖。数据目录与只读安装资源分开，目录移动不迁移图库所有权。

桌面便携目录可用 `--desktop /path/to/illustash-desktop` 加入本平台桌面程序及 `dependencies/desktop-Cargo.lock` 依赖清单，运行 `./desktop.sh --library ...` 或 `desktop.cmd ...` 时显式传入便携资源根；应用安装包用 `python3 packaging/build.py --output /tmp/illustash-base --desktop-resources packaging/desktop-resources` 暂存同一资源，再在 `desktop/` 执行 `cargo tauri build --config ../packaging/tauri-bundle.json`。macOS 安装资源为 `Illustash.app/Contents/Resources/resources/`；Windows 为 Tauri `resource_dir()/resources/`。桌面启动参数与 ready/退出由桌面生命周期实现负责，不用另一个 Python 服务。构建需要本平台 Rust/C 工具链和 Tauri CLI；macOS 打包需 Xcode 工具链，Windows 需 MSVC 和 WebView2。跨平台编译不等于对应平台运行验收。

研究形态运行 `python3 packaging/build.py --research --output /tmp/illustash-research`，在基础包之外加入预构建 `bin/illustash-iqdb`（不在安装后调用 Cargo）及原生计算程序 `bin/illustash-worker`。四类计算（research、index、provenance、recognition）共用这一个程序（2026-10-13 起，#37；此前每类各复制一份相同的 `illustash-<kind>`）：四个 `ILLUSTASH_<KIND>_WORKER` 都指向它，宿主每次启动时用 `ILLUSTASH_WORKER_KIND` 显式传入种类，每次任务仍是独立进程，请求/响应两参数协议不变。直接诊断写作 `ILLUSTASH_WORKER_KIND=index bin/illustash-worker --check`；没有种类时拒绝。文件名为 `illustash-<kind>` 的旧布局程序（旧安装、显式覆盖）仍按文件名分派，与变量不一致时拒绝。2026-10-05 起 macOS/Windows 的四类计算全部原生执行（含 `--check`），包内不带 Python 源码或解释器；识别与索引需要资源根下的 CPU ONNX Runtime，Windows 另需固定 OpenBLAS。计算包只能在 macOS、Windows 上构建（Linux 已冻结，见 [README](../README.md)）；适配器收到没有原生实现的请求时直接报错，没有解释器回退。便携计算包的 `start.sh` / `desktop.sh` 及对应 `.cmd` 为当前进程把四个 worker 变量指向 `bin/illustash-worker` 并配置安装模型根，显式环境配置优先。launcher 本身不执行 `--check` 或计算；依赖诊断须显式调用适配器或 `research.cmd --check`，打开图库后的任务与恢复沿用现有 App 编排。共享可写默认位置为 macOS 的 `~/Library/Application Support/Illustash/Shared` 或 Windows 的 `%LOCALAPPDATA%\Illustash\Shared`；不写系统环境。研究包仅交付快照适配器和计算依赖，不包含 HTTP 宿主；正式结果仍由 Rust 验证导入。适配器按任务路由到原生计算，不持有正式数据库写入边界。index 种类直接执行原生参考特征、clean/build/scan，需要统一模型/标签资源与 CPU ORT，`--check` 加载实际 CCIP 会话；算法与恢复边界见 [角色索引](ARCHITECTURE_DIRECTION.md#原生角色索引维护)。直接启动裸核心时仍只按显式环境启用计算；基础包的 launcher 不配置这些适配器。recognition 种类沿用既有 `ILLUSTASH_RECOGNITION_WORKER` 协议生成 WD/CCIP 结果包，复用既有预处理/阈值；Rust 验证、导入和发布。无私人锚点仍生成 WD 标签与两类向量；扩展角色声明需要显式部署锚点。

默认不带模型；`--research --models PATH` 可按白名单（`native/terms/models.json` 的 4 个文件）加入 WD、CCIP 两个 ONNX 特征模型、CCIP metrics 模型及参数到 `resources/models/`，计算与核心指纹共用这一份资源。发布用的安装包不带模型，见下文「发布用的小安装包」。每个计算包（`--research`，带不带模型都一样）都把 `packaging/model-licenses/` 里提交的模型许可原文按原字节放进 `resources/licenses/models/`，并在 manifest 里逐文件登记。原文缺失或与 `sources.json` 登记的哈希不符时，打包拒绝执行；详见 [第三方组件与许可](THIRD_PARTY.md#模型与数据)。原生识别/索引默认模型根为资源根的 `models/`；`ILLUSTASH_MODELS` 可绝对指定，原生相对覆盖以资源根解析。Windows 用 `--openblas-site DIR` 指向装有固定 NumPy wheel 的 site-packages，只从中提取登记的 OpenBLAS DLL 和 wheel 许可，不携带 Python；`--ort-dylib FILE --ort-licenses DIR` 加入 CPU ONNX Runtime。构建脚本不自动下载或安装系统软件。运行 `research.cmd --check` 检查四类适配器，`first-start.cmd` 用显式图片文件夹和全新的 `.pilib` 路径建库并启动；已有库用 `research.cmd --library PATH`。Windows 便携包路径含空格可重定位，不需要系统 Python/Cargo。WebView2 与 VC++ 运行库仍是系统前提。默认共享索引放 `%LOCALAPPDATA%\Illustash\Shared`，显式 `ILLUSTASH_SHARED` 可覆盖。图库、私人锚点、图片、账号和缓存不随包发布。附模型需遵守上游许可（WD Apache-2.0；CCIP 按 BigScience Open RAIL-M，带使用限制）。

原生 CPU ORT 可显式通过 `--ort-dylib FILE --ort-licenses DIR` 交付。动态库重命名为 `resources/onnxruntime/libonnxruntime.dylib`（Windows 为 `onnxruntime.dll`），沿用原生消费者的默认路径；`ILLUSTASH_ORT_DYLIB` 仍可覆盖。许可目录必须同时提供该分发的 `LICENSE` 和 `ThirdPartyNotices.txt`，按原字节放入 `resources/licenses/onnxruntime/` 并登记 SHA-256。脚本不下载库或模型；文件已复制不等于其平台、ABI 或推理通过验收。PocketFFT BSD 许可原文为 `native/terms/vendor/pocketfft/LICENSE.md`，随基础与计算资源安装到 `resources/licenses/pocketfft/LICENSE.md`。LittleCMS 2.19 与 lcms2 封装层的 MIT 许可 `native/terms/licenses/LCMS.txt` 随基础与计算资源安装到 `resources/licenses/`。本项目 `LICENSE`（GPL-3.0-or-later）与 `docs/THIRD_PARTY.md` 也随每个产物放入 `resources/licenses/`（2026-10-13 起，#8），GPLv3 第 7 条附加许可的正文在 `THIRD_PARTY.md` 里，随它分发（2026-10-22 起，#271）；Rust 依赖的许可正文 `resources/licenses/rust-dependencies.txt` 同样随每个产物分发，Windows、macOS 完整安装包暂存时列为必需组件（2026-10-16 起，#161）。全部第三方组件与许可汇总见 [第三方组件与许可](THIRD_PARTY.md)。

完整 macOS App 安装与各平台闭环以 #158 的实际门槛为准，不能用便携资源合成检查替代。


Windows MSVC 构建由仓库 `.cargo/config.toml` 统一启用 `+crt-static`；打包脚本的 Cargo 调用固定从仓库根执行，以读取该配置。完整安装包暂存前必须在 MSVC 开发者命令行中提供 `dumpbin`，脚本逐一检查计算目录的 exe 与 DLL（含延迟导入）；依赖 VCRUNTIME、MSVCP、CONCRT、VCOMP 或 MSVCR 的组件会拒绝打包。系统自带的 UCRT/API-set 依赖允许保留。Rust 的静态 CRT 配置不改变外部 ONNX Runtime DLL；从 Python wheel 提取的动态 CRT DLL 不能直接用于完整安装包。

Windows 的 ONNX Runtime 1.30.0 使用官方标签 `v1.30.0`（`f2c39fe2`），在对应源码根的 x64 MSVC 开发者命令行运行以下命令；需要 Python、CMake 3.28+、Ninja 和上游构建依赖。保留完整 CPU 算子，使用静态 CRT，不构建 Python 绑定；2026-10-21 起加 `--use_dml`，同一个库带上 DirectML 执行后端（#271），CPU 那条路的算子和输出与不加时相同。`--use_dml` 让构建从 NuGet 还原 `Microsoft.AI.DirectML` 1.15.4（版本由该标签的 `packages.config` 固定），包解压在 `ORT_BUILD/packages/Microsoft.AI.DirectML.1.15.4/`。把产出的 `ORT_BUILD/Release/onnxruntime.dll` 传给 `--ort-dylib`，该源码根的 `LICENSE` 与 `ThirdPartyNotices.txt` 传给 `--ort-licenses`，这个 DirectML 包目录传给 `--directml`：打包从里面取 `bin/x64-win/DirectML.dll`（放在 `onnxruntime.dll` 旁边，ORT 从自己所在的目录加载它）和 `LICENSE.txt`、`LICENSE-CODE.txt`、`ThirdPartyNotices.txt`。构建后仍需核对全部 DLL 依赖及真实模型推理，`--check` 通过不能代替推理验收。

```bat
python tools/ci_build/build.py --config Release --build_dir ORT_BUILD --build_shared_lib --parallel 4 --skip_tests --skip_submodule_sync --compile_no_warning_as_error --cmake_generator Ninja --use_dml --cmake_extra_defines CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded ONNX_USE_MSVC_STATIC_RUNTIME=ON protobuf_MSVC_STATIC_RUNTIME=ON ABSL_MSVC_STATIC_RUNTIME=ON gtest_force_shared_crt=OFF
```

**macOS 的 CPU ONNX Runtime**（#190）：用 PyPI 上 `onnxruntime` 1.30.0 的 wheel 里自带的库，#7、#192 虚拟机验收用的 macOS 完整包就是这样打的。wheel 标签 `cp312-cp312-macosx_14_0_arm64`（经 delocate 0.13.0 处理）；`--ort-dylib` 传 wheel 里的 `onnxruntime/capi/libonnxruntime.1.30.0.dylib`（Mach-O arm64，SHA-256 `b46713ea6a25a5227541c21001f6422c1ccdeaecc369aa026a61b752520325c3`），`--ort-licenses` 传 wheel 里的 `onnxruntime/` 包目录，`LICENSE` 和 `ThirdPartyNotices.txt` 都在那里。版本与上面 Windows 那份相同，但 Windows 是从源码构建的，macOS 直接用上游发布的 wheel。macOS 只需要 arm64（2026-10-20 用户定，只发 Apple 芯片版）。复现时取同一个 wheel，解压后先核对上面的哈希：

```sh
pip download onnxruntime==1.30.0 --no-deps --only-binary=:all: --platform macosx_14_0_arm64 --python-version 3.12 --implementation cp -d <目录>
unzip <目录>/onnxruntime-1.30.0-cp312-cp312-macosx_14_0_arm64.whl -d <解压目录>
shasum -a 256 <解压目录>/onnxruntime/capi/libonnxruntime.1.30.0.dylib
```

`packaging/build.py` 不下载，只把 `--ort-dylib` 给的文件改名为 `resources/onnxruntime/libonnxruntime.dylib`，并按原字节复制许可文件；宿主经 `ort` crate `=2.0.0-rc.10`（`load-dynamic`，见 `native/terms/Cargo.toml`）在运行时加载它。**遥测**（2026-11-13，#381）：这份官方 wheel 里的库内置微软的 1DS 遥测，默认开启。宿主经 `TaskContext::process_observed` 启动每个 worker 时都在子进程环境里设 `ORT_DISABLE_TELEMETRY=1`，worker 和合成驱动在 `main` 开头再设一次，`recognition_compute::session::initialise` 看不到这个值就拒绝加载，并用 `with_telemetry(false)` 调 `DisableTelemetryEvents`（`native/terms/src/recognition_compute/telemetry.rs`）。Windows 的自建库不带 1DS；它的 ETW provider 只登记在全零组里，不进 Windows 的遥测通道，所以构建时加不加 `--no_telemetry` 效果一样。2026-10-04 的 macOS arm64 合成对照用的也是 1.30.0（见 [识别结果包](RESULT_PACKAGES.md)）。`tests/requirements.txt` 的 `onnxruntime>=1.18,<2` 只管测试对照用的 Python 环境，不决定打包用的版本。

**Windows 完整安装包（2026-10-03）**：在 Windows 用 `packaging/build.py --research --models PATH --openblas-site DIR --ort-dylib FILE --ort-licenses DIR --directml DIR --output NEW_PATH --desktop-compute packaging/desktop-compute` 暂存，再在 `desktop/` 运行 `tauri build --bundles nsis --config ../packaging/tauri-full.json`。完整 NSIS 包的资源只在安装目录下的 `compute/`（计算程序 `illustash-worker`、核心工具、IQDB、`resources/`、模型、ONNX Runtime 与 DirectML.dll、固定 OpenBLAS；2026-10-05 起不含 Python 运行时与计算源码；2026-10-13 起不再另带顶层 `resources/`，见下文「产物布局」）；普通快捷方式在创建线程前为当前进程配置这些组件，核心与计算使用同一模型/标签资源根，共享可写数据默认在 `%LOCALAPPDATA%\Illustash\Shared`。显式环境配置优先，不修改系统环境；缺组件显示重装提示，不静默退回“未启用”。基础安装配置不携带 `compute/`，仍不启用计算；2026-10-12 起与 macOS 共用同一校验（#36）：只要存在 `compute/`，`manifest.json` 就必须可解析并声明 format 1、variant research、platform win32，必需组件须存在且登记在 files 中，否则启动报错并提示重装完整版或先卸载再装基础版。联网溯源、参考数据下载与账号授权保持独立；识别默认用 DirectML 推理，没有可用显卡时用 CPU（见 [装不上、打不开怎么办](INSTALL.md#windows)），不包含个人图库、锚点或凭据。回退时先卸载完整版再装基础版，避免残留完整包标记触发计算；图库与共享数据不作为安装资源卸载。

**macOS 完整应用资源布局（2026-10-04）**：在 macOS 将显式模型（发布用的小包不传，见下段）、ORT 动态库及许可一并传给 `packaging/build.py --research --models PATH --ort-dylib FILE --ort-licenses DIR --output NEW_PATH --macos-desktop-compute packaging/desktop-compute`，再在 `desktop/` 用 `tauri build --bundles app --config ../packaging/tauri-full-macos.json`。计算资源位于 `Illustash.app/Contents/Resources/compute/`，只复制便携包已有 manifest 登记且哈希一致的 bin/resources/dependencies，排除桌面主程序、快捷脚本和私人目录；缺资源或复制期间变化不发布新 manifest。完整 App 不再另带 `Contents/Resources/resources/`。普通应用进程在 Tauri/worker 线程启动前设置自身默认 ROOT/MODELS 和四个 worker 变量（同一 `illustash-worker`），SHARED 默认 `~/Library/Application Support/Illustash/Shared`；显式环境覆盖优先，只有基础包没有 compute 目录时保持基础能力，有 compute 而缺/坏 manifest、组件或执行权限会明确报错。2026-10-05 起计算资源不含 Python workers，四类计算全部原生；签名 App、Finder/GUI 与干净环境的日常闭环仍按 #158 单独验收。

**发布用的小安装包（2026-10-19，#168）**：用户 2026-10-16 决定，首个内测版起每个平台只发不含模型的安装包，模型在第一次使用时下载；带模型的完整包留给开发和测试。打法与上面两段相同，只是不传 `--models`（`--desktop-compute`、`--macos-desktop-compute` 不再要求它）；暂存时 4 个原始模型文件要么全有、要么全无，只有一部分时拒绝打包。macOS 开发完整包可另带 WD 执行变体，打包时校验其大小和 SHA-256；缺少变体仍可用原始 CPU。发布的小安装包通过首次模型下载取得变体。

- **桌面启动**（`desktop/src/bundled_workers.rs`）：compute 的 manifest 里 4 个模型都登记且存在，就是完整包，模型根为 `compute/resources/models`；一个都没登记，就是小包，模型根设为共享目录下的 `models/<模型包标识>`（macOS `~/Library/Application Support/Illustash/Shared/models/…`，Windows `%LOCALAPPDATA%\Illustash\Shared\models\…`；显式 `ILLUSTASH_SHARED` 时跟随它），并设 `ILLUSTASH_MODELS_ON_DEMAND=1`；只登记了一部分时报错，提示重装。显式 `ILLUSTASH_MODELS` 照旧优先。模型写在 App 包外，签名不受影响；同一模型包标识的目录被以后的版本沿用，程序更新不用重下。
- **下载与导入**（`native/terms/src/app_models.rs`，`GET/POST /api/models`）：清单 `native/terms/models.json` 编译进程序，写明模型包标识、每个文件的路径、大小、SHA-256，以及 Hugging Face（固定提交）和 ModelScope 的地址。只在用户点「下载」时开始；每个文件依次试 HF 地址经镜像（默认 `https://hf-mirror.com`，存在 `preferences.json` 的 `model_mirror`，只接受 https，测试可用本机 http 端口）和 ModelScope（测试用 `ILLUSTASH_MODELSCOPE_BASE` 指向本机端口）。HTTP 代理取 `HTTPS_PROXY`/`ALL_PROXY`，再取系统设置（macOS `scutil --proxy`，Windows 当前用户「Internet 选项」）；不读 PAC，本机地址不走代理。文件先写到模型根旁边的 `<模型根>.partial/<路径>.part`，断线后用 Range 续传（来源不支持时从头下），每个文件按大小和 SHA-256 核对，不符就丢弃并换下一个来源；原始模型齐了写 `pack.json` 并激活（模型根不存在时整目录改名，已存在时逐个文件移入），随后下载本平台可选的执行变体；变体失败或取消不影响已经激活的原始模型。从文件导入接受多个 `.onnx`、`.json` 或 `.zip`，按大小再按哈希认文件。`{"action": "delete"}` 删除已下载的模型（先把目录改名再删，连同未完成的部分），只在 `ILLUSTASH_MODELS_ON_DEMAND=1` 时可用，下载或导入进行中、或本会话正在同步识别、更新角色索引时拒绝。全应用同一时间只有一个下载或导入，切换图库不中断；开始前检查磁盘空间。
- **WD 执行变体**（2026-11-12，#375）：用户批准从项目 GitHub 模型 Release `wd-coreml-v1` 分发；仅 macOS Apple 芯片安装追加到下载列表，不改变原始模型包标识。已有安装通过设置页「下载 WD 加速文件」补齐；缺失或损坏时继续用原始 WD CPU，设置页说明原因并可重新下载或从文件导入。文件先按内置大小和 SHA-256 校验再激活，已有变体在下载/导入时也重新验哈希，因此同长度损坏可修复。首次下载前仍需用户点击，推理过程不联网。维护者使用 `tools/build_wd_coreml.py` 从固定原始模型生成发布文件，依赖版本、变换数量和最终 SHA-256 不符即拒绝输出；客户端不需要 Python。许可与变换说明见 [第三方组件与许可](THIRD_PARTY.md#模型与数据)。
- **没有模型时**（只在 `ILLUSTASH_MODELS_ON_DEMAND=1` 时生效）：同步照常导入图片，停在「等待识别」，原因写「识别模型还没下载」；检查新角色拒绝；首次引导的识别自检不启动进程。模型就位后页面自动同步一次，把等着的图识别掉。界面见 [Web 界面](WEB_V2.md#识别模型2026-10-19168)。
- **Windows 覆盖安装**：`packaging/nsis-full-hooks.nsh` 在复制前删除 `compute\resources\models`，在旧完整包上装小包时不留下没人用的模型；完整包会重新复制自己的模型。卸载不删共享目录里下载的模型，和其他共享数据一样。

**产物布局与资源归属（2026-10-13，#37）**：四类计算共用一个 `illustash-worker`，完整安装包的资源只放一份。各产物的文件与消费者：

| 内容 | 便携包 | 基础安装包 | 完整安装包 | 消费者 |
|---|---|---|---|---|
| `bin/illustash-terms` | 有 | 无（桌面程序自带命令行） | `compute/bin/` | 便携 `start.*`、`first-start.cmd`、用户命令行；完整包里计算不调用它，保留为控制台命令行入口（Windows 桌面程序是 GUI 子系统） |
| `bin/illustash-worker` | 计算包 1 份 | 无 | `compute/bin/` 1 份 | 宿主按种类启动；`research.cmd --check` |
| `bin/illustash-iqdb` | 计算包 | 无 | `compute/bin/` | provenance 种类在 worker 同目录找它算本地签名 |
| `resources/web/static/`、`resources/tags/tags_v3.csv` | `resources/` | `resources/` | 只在 `compute/resources/` | HTTP 界面、标签身份、识别与索引；完整安装启动时资源根设为 `compute/resources` |
| `resources/licenses/` | 有 | 有 | `compute/resources/licenses/` | 许可声明，运行时不读；manifest 逐文件登记 |
| 模型 | 计算包按参数 | 无 | 完整包在 `compute/resources/models/`；发布用的小包不带，下载到共享目录（#168） | 识别、索引 |
| ONNX Runtime、OpenBLAS | 计算包按参数 | 无 | `compute/resources/` | 识别、索引、研究 |
| `dependencies/*.lock`、`manifest.json` | 有 | 无 | `compute/` | 依赖版本清单；完整包启动校验（#36） |

同一组 release 二进制、单次测量（2026-10-13，带模型的完整形态）：macOS arm64 App 原始 737,478,042 → 706,733,139 字节（ditto ZIP 548,286,591 → 535,419,045）；Windows x64 安装后占用 755,375,174 → 722,559,639 字节，NSIS 安装包 524,888,704 → 525,292,844 字节，几乎不变：NSIS 整体压缩本来就把相同的 worker 副本压到接近零，收益只在磁盘占用和逐文件压缩的 ZIP。基础包只多了约 40 KB 许可文本。2026-10-16 起每个产物另加约 450 KB 的 Rust 依赖许可正文（#161）。

旧版完整包升级：macOS 整个 App 替换，没有残留。Windows NSIS 覆盖安装不删新包不再交付的文件，完整包用 `packaging/nsis-full-hooks.nsh` 在复制前删除旧布局的四个 `illustash-<kind>.exe` 和能认出是 Illustash 资源的顶层 `resources/`；不加这一步时，这些文件在覆盖安装和卸载后都会留在安装目录。回退到旧版本时安装旧版完整包，它自带自己需要的四个副本和顶层资源；新版留下的 `illustash-worker.exe` 旧版不使用（回退未实测）。

显式 `tests.platform_packaging_mac` 使用当轮 `ILLUSTASH_DESKTOP_BIN`、`ILLUSTASH_TERMS_BIN`、`ILLUSTASH_RESEARCH_ADAPTER_BIN`、`ILLUSTASH_IQDB_BINARY`，缺产物失败，不自动构建。它在隔离合成 `.app` 布局中执行桌面二进制的原生 CLI 分支，验证不传 `--root` 的计算资源启动、学习/恢复/人工保留和组件错误；模型/ORT 输入为 marker，不运行 Tauri GUI 或模型。签名、实际 `.app` 安装、Finder/GUI 和完整计算闭环仍按 #158 单独验收，保持现行签名与平台门槛。

原生研究计算候选 `native/terms/src/research_compute` 使用同一 `illustash-research/1` 快照协议，提供独立的特征归一化与版本指纹（每个 worker 进程只加载一次，不做跨调用缓存；非有限值、零范数和平方和溢出的有限超大向量均拒绝）、原型与弱线索计算、未识别池和共享 OC 分组；它不持有正式图库、历史或人工写入接口。`research-compute` 是显式构建的测试 example；安装适配器将受支持的学习、regroup 和 OC 请求交给同一原生 dispatcher。Windows 每进程先验证固定 ILP64 OpenBLAS 资源，缺失或身份不符会报错，不能回退到 portable 产物。其他平台的 portable 数值后端未经验收（Linux 已冻结）；原生 CPU 识别生产入口覆盖 macOS 与 Windows（2026-10-05）。宿主与识别 worker 共用这一平台 gate：仅原生安装入口强制新角色 sidecar，旧 Python 的无 sidecar 响应沿用公开 v1 接收；任何平台显式声明的 sidecar 仍须全量预验，不能静默退回仅发布 WD。显式测试 example 仍可独立验证原生候选，不意味着生产启用。相似/关系计算及含相似计算的混合 events 由同一 dispatcher 原生执行，规则见 [架构方向](ARCHITECTURE_DIRECTION.md)。macOS 的窄 float32 乘积由 Platform 调用系统 Accelerate，其他平台的数值保真需分别验收。用户于 2026-10-04 批准沿用当前 NumPy 1.26 标量 introsort 的并列排序合同，覆盖密度优先、距离与 trait 描述排序，不采用 NumPy 2.x 的 SIMD 并列顺序。用户于 2026-10-04 批准原型 p90 固定为 NumPy 1.26 的 float64 线性插值口径：float32 spread 排序，索引 `(n-1)*0.9` 和插值比例为 float64，端点差先按 float32 相减，再转 float64；比例 ≥0.5 时从上端点反向插值，随后按原 floor/cap、负例边距和 hint 规则计算，不增加容差。旧 NumPy 2.x 的标量提升会在少数弱候选临界输入给出不同值，这一版本差异保留为对照证据，不修改人工结论或旧 golden。不同 BLAS 后端的乘积位差也会在相同原型和阈值下改变临界强认领/弱线索，float64 插值的批准不能代替各平台数值后端契约的确定和验收。基础 NPY 保留 C/F 布局及 float32/float64（含大小端）的归一化精度，再转为 float32 并哈希；其他 dtype 当前显式拒绝，未取得完整 `np.load` 输入域兼容验收。不能把 floor/cap 限定的合成通过当作整个学习迁移完成；正式任务验证/发布、人工覆盖与撤销继续由现有 App/Core 持有。

Windows 原生研究的资源身份只在 `crates/platform/src/research_openblas.json` 登记，版本与对应 NumPy 参考不可互换。`--openblas-site` 只从显式 site-packages 的 `numpy.libs` 提取唯一登记 DLL，并复制同版本 wheel 的完整 `dist-info/licenses` 原字节树到 `resources/licenses/openblas`；缺少许可、身份不符、多个登记 DLL 或复制变化均拒绝发布包 manifest。安装资源固定为 `resources/openblas/openblas.dll`，普通 cmd launcher 和桌面 bootstrap 为当前进程设置 `ILLUSTASH_RESEARCH_OPENBLAS_DLL` 默认值，保留显式覆盖；平台加载器也可从绝对 `ILLUSTASH_ROOT` 解析同一路径，两者都未设置时 worker 按自身安装位置解析。运行时不搜索 Python 目录或 PATH。加载器在调用导出前用规范模块路径、卷序列号和 128 位文件 ID 复核实际 HMODULE 与已哈希锁定文件的身份，固定 C 枚举 i32、尺寸/步长 signed i64，完整解析 f32/f64 DOT/GEMV/GEMM/SYRK 及配置后持有模块和禁止写入/替换的文件句柄；加载后检查不能撤销已运行的 DllMain。Windows trait profile 保留 f64 权重和除法，norm 复用连续 pairwise 归约，alike 按同缓冲整矩阵乘积选择 DSYRK；精度和最终分类独立验证，不能以 f32 通过代替。各自对应 NumPy/DLL 的乘积逐位参考须同时记录 CPU 内核和实际线程数；scalar 排序与 percentile 使用上述已批准的独立合同。固定资源错误沿既有任务状态报告，正式发布仍由 App/Core 持有；原生学习/分组路径不代表识别、真实模型或 GUI 安装闭环已验收。

Windows 计算任务使用挂起创建 → 私有 kill-on-close Job → 恢复主线程，父 worker 先退出也终止并等待后代，宿主异常退出由 OS 关闭 Job 清理。共享索引与溯源租约使用可自动释放的文件锁。全新参考索引没有已有锚点时按空集比较；坏图可跳过，模型/运行时错误直接失败，不伪装成参考图不一致。`--check` 只验证依赖导入，不证明推理或真实图片准确率。

独立产物的专项验证与完整平台门槛分开：干净 macOS 由 #71 承接；Windows 的构建、安装和基础功能已由 #72 在开发机上实测，用户 2026-10-02 取消了 #90 的无 Python 干净 Windows 专项复验；取消关闭不计为该环境验收通过。打包脚本成功不代表这些门槛已通过。

## 识别的执行后端

2026-10-21 起（#270、#271）。识别和索引 worker 的 ONNX 会话按平台选执行后端（`native/terms/src/recognition_compute/backend.rs`），CPU 是参考。CPU 会话中，WD 在 Windows 上用 ONNX Runtime 默认 intra-op 线程数，其他平台用 4 个；CCIP 使用 runtime 默认线程数（`recognition_compute/session.rs`）。后端选择、加速开关、缓存与回退规则只在本节维护：

- **macOS（Apple 芯片）**：WD 与 CCIP 都走 Core ML（MLProgram 格式、全部计算单元，batch 固定为 1）。WD 使用单独校验的执行变体 `coreml/wd-swinv2-v3-v1.onnx`（#375）；原始图的分区与高维操作阻碍加速，变体通过固定形状与等价图改写适配 Core ML。原始 `wd_swinv2_v3_emb.onnx` 保留，识别包、角色锚点与数据库中的模型 SHA-256、版本和标签表身份仍取原始文件，Windows DirectML 与 CPU 继续用原始图。变体的 revision、来源模型 SHA-256、大小和输出 SHA-256 编入 `models.json`，任何不符都回退原始 CPU。打包不变：现在带的 PyPI `onnxruntime` 1.30.0 arm64 动态库自带 Core ML 执行后端，ort `load-dynamic` 下直接注册。
- **Windows（x64）**：WD 和 CCIP 都走 DirectML（`recognition_compute/directml.rs`）。会话照 ORT 对 DirectML 的要求关闭 memory pattern、按顺序执行，动态 batch 维固定为 1；经 `OrtDmlApi` 的 `SessionOptionsAppendExecutionProvider_DML2` 按「高性能」选 GPU，有核显和独显时用独显（旧的 `device_id` 接口按适配器序号，0 是接显示器的那块，在混合模式的笔记本上通常是核显）。需要支持 DirectX 12 的显卡。用的是自建的 1.30.0（加 `--use_dml`，见「独立安装与可选计算包」），它从自己所在的目录加载随包的 `DirectML.dll`。`ILLUSTASH_DIRECTML_GPU=minimum_power` 改选省电的那块 GPU，只用于在双显卡机器上测核显。**只有核显的机器没有测过**（2026-10-21 的验收机处在独显直连模式，核显不可见）。
- **运行库声明后端**（#271）：ONNX Runtime 库旁边可以放一个 `backends.json`，如 `{"format": 1, "wd": "directml", "ccip": "directml"}`，声明这个运行库给每个模型用哪个后端（`cpu`、`coreml`、`directml`）；有它时以它为准，没有时用上面的平台默认值，文件坏了或写了不认识的后端就全部用 CPU。Windows 完整包带这个文件（`packaging/onnxruntime/windows-backends.json`），macOS 不带。运行库和它的后端是一套配置：以后换成别的运行库（比如 Windows ML），换掉 `resources/onnxruntime/` 和这个文件即可，识别代码不用改；新的执行后端种类仍要在 `backend.rs` 里加一个分支。
- **编译缓存**：`<共享目录>/coreml-cache/<模型>/<键>/`（默认 `~/Library/Application Support/Illustash/Shared/coreml-cache/`）。键是模型文件、ONNX Runtime 动态库和 `SystemVersion.plist` 内容的 SHA-256，任何一个变了就换一个文件夹，在下次建会话时只删除该模型的旧缓存；WD 与 CCIP 各自保留有效缓存。旧单层布局的缓存在升级后淘汰并重新编译一次（2026-11-12，#374）。第一次编译 CCIP 约 4 s，之后每个 worker 加载约 0.65 s；识别 worker 和索引 worker 同时启动时用共享的 `compile.lock` 排队，只编译一次；锁文件长期保留，避免等待中的进程与后来的进程锁住不同文件。
- **退回 CPU**：加速会话建不起来，或运行时出错、输出校验不过时，这一张在新建的 CPU 会话上重算，这个 worker 余下的都用原始模型 CPU（WD 不在 CPU 上运行派生图）；只删失败模型的 Core ML 缓存文件夹（它可能就是坏的），下次 worker 重新编译。DirectML 退回的原因去掉了 ORT 错误开头的源码位置，只留提示，比如没有可用显卡时是「No devices detected that match the filter criteria.」，运行库不带 DirectML 时是「Specified provider is not supported.」。每个模型请求的、实际用的后端和原因记在 `<共享目录>/acceleration.json`，`GET /api/models` 的 `acceleration.last` 返回它，只去掉写于 WD 加速文件装好之前的那条 WD 回退记录（比较记录的 `at` 与加速文件的修改时间），设置页据此提示（[Web 界面](WEB_V2.md#识别模型2026-10-19168)）。模型或运行库本身坏了（CPU 也加载不了）不算加速失败：报 CPU 的错误，不写回退记录，清理该模型的空缓存目录；共享编译锁和其他模型的缓存保留。
- **开关**：`preferences.json` 的 `acceleration`（默认开，「设置 › 数据 › 识别模型 › 使用 GPU 加速识别」）；`ILLUSTASH_CPU_ONLY=1` 强制全部用 CPU，优先于设置，给逐位验收和排查用。关掉时会话的建法与 #270 之前完全相同。
- **结果**：已验收样本中，加速后端与 CPU 的差别是 fp32 舍入级（#213：CCIP 余弦 ≥ 0.99999999999958，角色距离最多变 1.3e-7，判定和最近邻不变）。已有图库的向量是 CPU 算的，换后端后同一张图重新识别，向量不再逐位相同；验收样本中的判定不变。研究计算的固定期望和 `tests/fixtures` 的录制重放用提交的向量，不跑模型，不受影响。`tests.platform_recognition_models`、`tests.platform_index_models` 设 `ILLUSTASH_CPU_ONLY=1` 与 Python ORT CPU 参考逐位比较；`tests.platform_recognition_models.AcceleratedRecognitionTest` 在有加速后端的平台上按容差比较（CCIP 余弦 ≥ 0.9999999999，距离和置信度差 ≤ 1e-6，认领的角色和最近邻不变；WD 在 macOS Core ML 与 Windows DirectML 上概率差 ≤ 2e-5、向量余弦 ≥ 0.9999999999，保存的标签只允许 0.001 边界上的进出和第四位小数差一，各判定阈值以上的标签不变），并在 macOS 上验证缓存损坏时退回 CPU。性能见 [性能](TERMS_PERFORMANCE.md#6-整应用识别吞吐基准)。

## Windows

2026-10-01 在 x64 Windows 11 家庭版开发机上实测（#72）。这台机器装有系统级 Python 3.12，所以只证明产品进程树不调用 Python，不等于干净环境；干净环境复验见 #90。

**构建**：需要 MSVC 生成工具（VC 工具集和 Windows SDK）、Rust stable（`x86_64-pc-windows-msvc`）、Tauri CLI 2 和 WebView2；`packaging/build.py` 遵守 `CARGO_TARGET_DIR`。`tauri build --bundles nsis,msi --config ../packaging/tauri-bundle.json` 产出 NSIS 和 MSI，安装到 `C:\Program Files\Illustash\`，资源在其下的 `resources\`。NSIS 安装界面按 Windows 显示语言用简体中文或英文，MSI 只有英文，见 [界面语言](WEB_V2.md#界面语言2026-10-18173)。NSIS 需要 Tauri 官方发布的 NSIS 工具及 `nsis_tauri_utils.dll`；直连下载超时可在构建机下载并按 bundler 固定校验值核对，放入工具缓存，不关闭校验或修改系统安全策略。仓库用 `.gitattributes` 固定 LF：Git for Windows 默认 `core.autocrlf=true`，否则签出后静态资源、标签表和测试夹具的字节都会变。

**运行时行为**：
- Windows 不创建原生顶部菜单，应用内侧栏和导航保留；macOS 保留原有原生菜单。帮助、按钮提示及动态撤销等按平台显示 `Ctrl` 或 `⌘`。
- 应用数据目录：未设置 `ILLUSTASH_APP_SUPPORT` 时为 `%APPDATA%\Illustash\`（macOS 为 `~/Library/Application Support/Illustash/`），保存最近图库记录和账号数据。账号凭据使用当前用户 DPAPI；存储及旧文件处理见 [架构参考](ARCHITECTURE_DIRECTION.md#原生账号与桌面登录桥)。
- 用户主目录取 `USERPROFILE`；默认图库位置和导出目录与 macOS 相同，即 `图片\Illustash`、`下载`。
- 写入配置和最近图库记录的路径经 `app_paths::canonical` 去掉 `\\?\` 前缀；超长或包含 Win32 不接受的组件时保留扩展形式。
- 退出：CLI 宿主响应 Ctrl+C、Ctrl+Break 和关闭控制台（`SetConsoleCtrlHandler`）；桌面通过 stdin EOF 通知宿主收尾，宿主同时挂在桌面的 Job Object 下，桌面异常退出时由系统回收。
- 归档导入按原始字节拒绝条目名里的反斜杠，与平台无关。
- 可选研究/索引/识别/溯源 worker 在 Windows 上可显式启用，依赖、模型和生命周期见「独立安装与可选计算包」。Apple 照片来源仍仅限 macOS。
- **SQLite 路径**：Windows 的文件连接统一使用内置 `win32-longpath` VFS，数据库与 `ATTACH` 文件名在 SQLite 边界转为扩展绝对路径；配置与展示路径的规则不变。不依赖缩短 `TEMP` 或启用系统长路径策略。升级暂存仍在包内，升级前备份与双库原子恢复约定不变。

**验收**：`python tools/windows_acceptance.py --bin <基础版 bin\illustash-terms.exe> --desktop "C:\Program Files\Illustash\illustash-desktop.exe" --work <空目录>`，需要管理员权限，桌面一项还需要管理员已登录本机控制台。脚本只用合成数据：被测进程的 PATH 里没有 Python，用户目录和应用数据目录都指向合成目录，用 WMI 进程启动事件记录整个产品进程树。覆盖 CLI 建库、文件夹同步、缩略图（EXIF 方向、透明、大图、中文路径）、详情、连续撤销、Host/CSRF 拒绝、备份、运行中切库与令牌轮换、导出导入、CTRL_BREAK 与 stdin 退出、端口冲突、强杀后重新打开、worker 缺席，以及已安装桌面的启动、截图、关窗退出和无残留进程。2026-10-01 的结果为 51/51 通过，产品进程树里只出现 `illustash-desktop.exe` 和 `msedgewebview2.exe`。同一台机器上，合成 11 图、release 构建的 `GET /api/assets` 三轮各 100 次，轮中位数的中位数为 15.4 ms，宿主峰值工作集约 10 MiB。只作记录，不与 macOS 比较。

**2026-10-02 长路径验收（#91）**：Windows 11 x64，通过 SSH 在用户默认 `TEMP` 下运行合成后端测试；224–225 字符图库路径的建库、旧库升级、WAL 手动备份（文件路径 270 字符）与恢复通过，升级前应用/source 双库恢复保持完整 schema（表、索引、视图、触发器）、类型化数据和 rowid；中文路径超过默认 VFS 的 1,040 字节缓冲也通过。旧库升级含提交失败回滚与进程强退恢复共 5 项，SQLite 定向测试 2 项。未验证 Windows 界面操作、UNC 网络共享或干净系统。Windows release 合成空库建库三轮各七对有效样本、每轮排除一对预热：延迟中位数的中位数变化 -0.43%，1 ms 采样的根进程峰值工作集变化 -0.047 MiB，按用户阈值直接接受；不代表真实大库或整个桌面进程树。另以同一对 release 的合成空库，三轮各 31 对有效交错 HTTP 请求、每轮每种查询排除五对预热：浏览/计数/设置最大延迟增加 1.69%，根进程峰值工作集减少 0.012 MiB，也按用户阈值接受。回退到旧程序时长路径限制会恢复，应先把包复制到较短路径，并按数据库格式使用升级前备份。

2026-10-02 Windows 11 x64 合成验收确认无原生菜单、保留导航，Ctrl+K 打开角色跳转，帮助显示 Ctrl 提示。DPAPI 的用途隔离、密文持久化、重启、替换、损坏拒绝与解绑通过；旧明文不读取、不迁移。NSIS 与 MSI 均能构建；NSIS 默认固定为当前用户安装。为避免正式 NSIS 自动迁移卸载已有 MSI，安装、资源读取、窗口及卸载用相同代码资源的独立合成产品标识验证，已有 MSI 字节与注册保留。正式 MSI → NSIS 迁移、跨 Windows 用户解密与真实凭据未验证；详细结果及首轮失败见 PR #103。回退安装程序与资源须成套替换；Windows 凭据回退规则见架构参考。

同机 `5d55cc6a` 基线与 `aad3554` 集成版本、11 个合成文件（含一张故意损坏图片）、3 轮每轮每端点每版本 100 次交错请求（另去掉各轮一次预热）：accounts 中位数 3.541 → 4.053 ms（+14.45%），assets 15.834 → 15.862 ms（+0.18%）；原生宿主工作集增加 1.96 MiB、峰值工作集增加 1.94 MiB，按用户小于 20% / 10 MiB 的规则接受。不代表真实大库、跨机器或整个桌面进程树。

回退时替换程序与资源为同一版本；数据库不兼容时按升级前备份恢复，不能直接用旧程序打开新格式。

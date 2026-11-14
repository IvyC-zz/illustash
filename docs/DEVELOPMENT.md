# 开发说明

状态：现行参考，2026-11-15；「发布」一节已按 v0.1.0 的实际发布修订（#190）。从 README 移出的开发者内容：构建、运行形态、图库包结构、目录、文档地图与检查流程。面向使用者的介绍见 [README](../README.md)。

核心是 Rust：`crates/core` 管业务规则、证据、人工裁决、关系、历史与投影，`crates/platform` 管平台能力，`crates/iqdb` 在本地算 IQDB 签名；`native/terms` 是应用宿主（CLI、HTTP、任务编排），`desktop/` 是只做启动与登录窗口的 Tauri 外壳。数据在 SQLite，推理用 ONNX Runtime（Apple 芯片的 macOS 上走 Core ML、Windows 上走 DirectML，CPU 是参考与回退，见[识别的执行后端](BACKEND_TASKS.md#识别的执行后端)），模型不进仓库、打包时按白名单加入。

文档、代码注释和提交信息里的 `#N` 编号、`PR #N` 与短提交哈希，凡早于 2026-10-06 开源的，都指向开源前的私有开发历史，在本仓库中查不到；本仓库的 Issue 和 PR 从 #1 重新编号。

## 三种形态

| 形态 | 入口 | 要点 |
|---|---|---|
| 本地 Web | `./start-web.command --library /路径/图库.pilib` | macOS 可双击；省略 `--library` 打开最近登记的图库；默认端口 8765，终端打印带一次性码的地址并用默认浏览器打开，页面只能从这个地址进入；终端 Ctrl+C 停止 |
| 桌面 App | `desktop/`（Tauri 外壳） | 没有能打开的图库时显示欢迎页，可在界面里新建、打开或从导出包导入；承载 Pixiv 登录窗口 |
| 安装包 | `python3 packaging/build.py --output OUT` | 基础版只含核心、静态界面与标签表；`--research` 追加四类原生计算适配器 |

### 快速开始

Core、Platform、宿主、桌面和 `crates/relation-fft` 共用根目录的 Cargo workspace、`Cargo.lock` 和 `target/`；FFT crate 只负责 PocketFFT 的 C++ 编译与原始接口。仍可用 `--manifest-path` 单独构建成员。依赖、features 和架构方向保留在各自 manifest；根配置统一 profile 与 tiny_http patch。开发构建沿用 terms 的第三方依赖 opt-level 3，自有成员和 tiny_http 保持 opt-level 0；自有成员带行号表，第三方不带调试信息。release 仅 desktop 去符号。

IQDB 保留独立的 `crates/iqdb/Cargo.lock`、`crates/iqdb/target/` 和原 profile（dev 自有代码带行号表、第三方不带调试信息，release 去符号）：其 image 0.25.10 / zune-jpeg 0.5.15 与宿主固定的 image 0.25.6 无法在同一 workspace 同时解析；合并会改变既有 JPEG 签名，用户于 2026-11-15 批准此例外（#311）。Windows 静态 CRT 对两个项目都仍由根 `.cargo/config.toml` 控制。

URL 解析统一沿用桌面原有的 `idna_adapter` 1.2.2 / ICU 2.3。独立宿主及便携包的 `start.sh` / `start.cmd` 从 `idna_adapter` 1.2.0 / ICU 1.5 切换后，可接受的 Unicode 域名范围扩大；桌面窗口内的宿主原已采用新版本。用户于 2026-11-15 批准此行为变化（#311），不增加旧版拒绝规则的兼容层；应用层的 URL 校验、配置保存方式、域名白名单和联网授权规则不变。

```sh
# 构建核心
cargo build --release --manifest-path native/terms/Cargo.toml

# 首次建库：只登记来源，不扫描图片
./target/release/illustash-terms library-create \
    --dest ~/Pictures/Illustash/我的图库.pilib --folder /图片目录

# 打开
./start-web.command --library ~/Pictures/Illustash/我的图库.pilib
```

宿主就绪时打印 `图库已启动：http://127.0.0.1:端口/?k=…`。这个地址只能用一次：第一次打开时换成会话 cookie，之后每个请求都要带它，没有就 403（见 [架构方向](ARCHITECTURE_DIRECTION.md) 中「原生宿主完成启动恢复」一段）。浏览器关掉重开丢了 cookie，就重启宿主拿新地址。

浏览器走查不打开真实图库：`./start-web.command` 不带 `--library` 时打开的是最近登记的图库，也就是自己日常用的那个。走查用 `python3 tools/dev_preview.py`，代理会话用 `preview_start {name: "web-isolated"}`（`.claude/launch.json` 里只有这一项）。脚本把 `HOME`、`ILLUSTASH_APP_SUPPORT`、`ILLUSTASH_SHARED` 指向一个临时目录，设 `ILLUSTASH_NO_KEYCHAIN=1` 不碰钥匙串，在里面新建一个只含生成渐变图的图库，宿主跑在空闲端口上；`8777` 端口只做一次跳转，转到宿主打印的带码地址。退出时删除临时目录，`--work DIR` 改用指定目录并保留；宿主用 `target` 里较新的构建，或 `ILLUSTASH_TERMS_BIN`。自己另起宿主走查时，同样要把这三个目录指向临时位置，再用 `preview_start {url}` 打开就绪行里的地址。

`start-web.command` 在 `target/{release,debug}` 里找 `illustash-terms`，也可以用 `ILLUSTASH_TERMS_BIN` 指定，找不到直接报错。服务和桌面入口都不隐式建库或升级；开库时按已保存的设置自动同步，`--no-auto-sync` 可关闭自动同步。多来源、Apple Photos 来源与出处分组见 [图库](LIBRARIES.md)。

### 启动参数

`illustash-terms http --help` 的现行参数：

| 参数 | 作用 |
|---|---|
| `--library PACKAGE` | 要打开的 `.pilib`；省略时打开最近登记的图库，没有记录则提示指定 |
| `--root ROOT` | 资源根（`web/static`、`tags/`）；默认取可执行文件相邻的 `resources/`，`ILLUSTASH_ROOT` 可覆盖 |
| `--port PORT` | 监听端口，默认 8765 |
| `--backup` | 用 SQLite backup API 备份后退出，允许服务仍在运行 |
| `--upgrade` | 先备份，再在一个事务里迁移；与 `--backup` 互斥，普通启动不升级 |
| `--no-sync` / `--no-auto-sync` | 禁用本次会话的同步 / 只关闭自动同步，手动同步仍可用 |
| `--sync-limit N` | 每轮同步最多识别 N 张，取最近保存的 N 张（[识别顺序](ARCHITECTURE_DIRECTION.md#原生来源同步与应用刷新)），0 不限；不限制来源扫描 |
| `--no-names` / `--names-network` | 关闭名字同步 / 本次进程允许名称文字查询联网（图库也可在界面上持久授权） |
| `--no-provenance` / `--allow-provenance-network` | 不跑溯源任务 / 允许溯源联网 |
| `--no-index-check` / `--index-check` | 关闭 / 打开开库时的角色索引检查（默认自动） |
| `--index-network` | 索引检查允许联网 |
| `--open` / `--no-open` | 就绪后（不）用默认浏览器打开带一次性码的地址，只在 macOS 上打开；默认不打开，`start-web.command` 默认带 `--open` |
| `--native` | 兼容参数，原生入口不做额外动作 |

已退役、直接报错的参数：`--python`、`--upstream`、`--route`、`--refresh`、`--source`、`--db`、`--cache`、`--identity`、`--no-similar` 和裸 `--`。恢复旧版本要停服务、保留当前包与 WAL，再从相容版本的升级前备份恢复；不要复制正在使用的 SQLite 文件。`tools/build_db.py` 重建的是分析源库，不是应用的更新入口。

### 本地运行诊断

`native/terms/src/diagnostics.rs` 使用固定枚举、数值和随机错误编号记录故障，不收集错误原文或请求内容。记录位于应用支持目录的 `diagnostics/`，与图库数据库分离；用非阻塞跨进程文件锁串行写入、按条目数/大小/期限清理。macOS 新目录和文件分别为 0700/0600；Windows 继承用户应用数据目录权限。读取时重新按白名单解析，损坏或未知格式的记录不导出。接口以外的路径收到的输入错误（如 WebView2 每次加载页面都会请求的 `/favicon.ico`）照常应答 400，但不记录、不带诊断响应头；其余失败照常记录。删除该目录只清除诊断，不影响图库。

HTTP 错误保留原有状态码与 `error` 正文，额外通过 `X-Illustash-Diagnostic` 返回 `id/stored/publication/retry`；后台失败在状态中附加 `diagnostic`。读取失败标记不涉及提交；传输层无法证明写入边界时标记未知，导入任务依据发布点区分未提交与已提交，其余分批任务保守标记可能部分提交。记录失败不改变业务 `Result`。认证后的 `/api/diagnostics` 和仅启动页有权调用的 `startup_diagnostics` 提供同一份脱敏报告；外壳记录宿主启动/退出及超时，但不导出 stderr。用户入口与保留上限见 [报告问题](INSTALL.md#报告问题)。

### 安装包与桌面

版本号只在 `native/terms/application.json` 写一处（SemVer，可带 `-beta.1` 这类预发布后缀，不带 `+build`；关于页的版本与渠道也读这里）。`python3 tools/version.py --set X.Y.Z` 改它并同步 `desktop/tauri.conf.json`、各自有 crate 的 `Cargo.toml` 和根及 IQDB 的 `Cargo.lock` 里的本项目包；`--sync` 只按它同步；不带参数只检查。本机全量与 CI 的 Python 测试会检查一致，`packaging/build.py` 不一致就拒绝打包。改版本号会改动 `desktop/tauri.conf.json`，按 AGENTS.md 同步 `tests/migration_inventory.json` 的指纹。

`packaging/build.py` 按白名单复制资源，生成逐文件 SHA-256 的 `manifest.json` 与依赖清单；`--archive` 出 ZIP，`--binaries` 复用已构建的二进制，`--profile dev` 用调试构建。研究便携形态加 `--research --models PATH`；macOS 与 Windows 另需 `--ort-dylib FILE --ort-licenses DIR`，Windows 还需 `--openblas-site DIR`；桌面形态加 `--desktop PATH`，或加 `--desktop-resources` 后再在 `desktop/` 执行 `cargo tauri build --config ../packaging/tauri-bundle.json`。

2026-10-05 起 macOS 与 Windows 的四类计算（识别、索引、学习/相似、溯源）全部原生执行，包内不带 Python 与计算源码；2026-10-13 起四类共用一个 `bin/illustash-worker`，宿主用 `ILLUSTASH_WORKER_KIND` 显式传入种类；Linux 不提供计算包（已冻结，见 [README](../README.md)）。完整安装包、平台门槛与依赖边界见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)。

### 发布

2026-10-16 用户定（#165）：先手动发一两次，再决定哪些步骤做成脚本；更新说明只写在 GitHub Release 正文，仓库里不放 CHANGELOG.md。**推 tag、建 Release、把 Release 草稿公开，每一步都要用户明确授权**；发布与否和时间也由用户决定。升版本号的 PR 由开发会话做，全量和合入由集成会话做，打包、验收和发布由负责这次发布的会话做。验收结果、安装包哈希写在这次发布的 Issue 里，不进仓库。

1. **定版本号**：SemVer，tag 写作 `vX.Y.Z`，内测包可加预发布后缀（`0.2.0-beta.1`）。凡是升级图库格式的版本至少升次版本号。是否升级图库格式，对照上一个 tag 和发布提交的三个格式版本号：词条 schema（`crates/core/src/schema.rs` 的 `SCHEMA_VERSION`）、应用 schema（`native/terms/src/app_library.rs` 的 `APPLICATION_SCHEMA`）、包格式（同一文件的 `FORMAT`），任何一个变了就是：

   ```sh
   git grep -n -E 'pub const (SCHEMA_VERSION|APPLICATION_SCHEMA|FORMAT):' <上一个tag> <发布提交> -- crates/core/src/schema.rs native/terms/src/app_library.rs
   ```

   首个公开测试版没有上一个 tag，用现在的 `0.1.0`（#165）。关于页的渠道「预览版」是 `application.json` 的 `channel`，与检查更新无关；检查更新只看版本号带不带后缀（第 10 步），所以装了不带后缀版本的人收不到带后缀的版本。
2. **升版本号并同步**：从最新 main 开分支，`python3 tools/version.py --set X.Y.Z` 改 `native/terms/application.json`，并同步 `desktop/tauri.conf.json`、六个自有 crate 的 `Cargo.toml` 和根及 IQDB 的 `Cargo.lock` 里本项目的包；再运行 `python -m tools.migration_inventory sync-sources` 同步迁移清单指纹。`python3 tools/version.py` 不带参数只检查：一致时打印「版本号一致：X.Y.Z」，不一致时逐处列出并退出 1。另外两道检查：全量里的 `tests/test_version.py`，以及 `packaging/build.py` 不一致就拒绝打包。这个 PR 按普通代码级别走合并队列，转 Ready 写「待集成」。版本号已经是要发的值时跳过这一步。
3. **发布提交与本机全量**：集成会话把版本号 PR 编进一批，在候选 head 上经 `collab_run.py run -- bash tools/check_all.sh` 跑全量（占 heavy 槽，做法见 [合并队列](LOCAL_CHECKS.md#合并队列集成会话)），用户批准后合入。发布提交就是这一批合完后的 main 提交，它的 tree 与通过全量的候选 head 相同（`git rev-parse <发布提交>^{tree} <候选head>^{tree}` 两行一样）。main 在那之后又合入了不跑全量的 PR（界面级、只改文档），要带上它们就请集成会话在 main 当前提交上补跑一次全量，以它为发布提交。
   - **依赖漏洞**：在发布提交上对根与 IQDB 两份锁文件分别运行 `cargo audit -f Cargo.lock`、`cargo audit -f crates/iqdb/Cargo.lock`。有漏洞时逐条判断是否用到受影响的功能：用到的先升级再发布（升级 PR 按普通代码级别走合并队列，发布提交随之换成合入后的 main 提交），没用到的把编号和理由写进这次发布的 Issue。unmaintained、yanked 之类的警告只记录。需要 `cargo install cargo-audit --locked`（本机已装 0.22.2）；第一次运行会从 GitHub 拉 RustSec 漏洞库。
   - **更新公钥**（#279）：发布提交里 `desktop/tauri.conf.json` 的 `plugins.updater.pubkey` 不能是空的，并且和用户保管的私钥是一对（与 `~/.tauri/illustash-updater.key.pub` 的内容相同，见下面 [更新签名密钥](#更新签名密钥)）。公钥是空的构建照样能用，只是「下载并重启」不出现，只能打开下载页；公钥不对的构建以后收不到任何自动更新。
   - **仓库 ID**（#358）：`gh api repos/shio-vinyl/illustash --jq .id` 的结果要等于发布提交里 `native/terms/src/app_updates.rs` 的 `REPOSITORY_ID`。ID 不对时应用靠按名字的回退照样查得到更新，验收看不出来，但仓库以后改名或转移就接不住了（见第 10 步）。仓库删掉重建过、或者换了仓库，都要先改这个常量和本文、[Web 界面](WEB_V2.md#界面语言2026-10-18173) 里的地址。
4. **两个平台打包**：首个内测版起每个平台只发不含模型的小安装包，模型第一次使用时下载（#168）；打法与完整包相同，只是不传 `--models`，参数说明见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)。两边都从发布提交的干净检出构建（例如 `git worktree add --detach <目录> <发布提交>`）：关于页的构建号取自 `git rev-parse --short HEAD`，已跟踪文件有改动时带 `-dirty`，发出去的包不能带。短哈希的长度由各自检出里的对象数决定，两个平台可能不同（0.1.0：macOS `1b72ffd4`，Windows `1b72ffd`），指的是同一个提交。release 构建按 [本机资源](LOCAL_CHECKS.md#本机资源排队计时与后台优先级) 的规定排队。安装包文件名用 Tauri 的默认格式：`Illustash_X.Y.Z_aarch64.zip`、`Illustash_X.Y.Z_x64-setup.exe`（2026-10-20 用户定）。
   - **macOS**：只发 Apple 芯片（arm64）版，有 Intel 用户反馈时再考虑通用二进制（2026-10-20 用户定）。`collab_run.py run -- python3 packaging/build.py --research --ort-dylib FILE --ort-licenses DIR --output NEW_PATH --macos-desktop-compute packaging/desktop-compute`，再在 `desktop/` 运行 `collab_run.py run -- cargo tauri build --bundles app --config ../packaging/tauri-full-macos.json`。产物在 `target/release/bundle/macos/Illustash.app`，只有链接器给主程序加的临时签名，整个包没有封签：`codesign --verify --deep --strict` 报 “code has no resources but signature indicates they must be present”。以前虚拟机验收用的包都补签过，没有验证过不补签的包在 Gatekeeper 下的表现，所以先拷出来补签整个包：`codesign --force --deep -s - Illustash.app`，再用 `codesign --verify --deep --strict Illustash.app` 核对通过（`codesign -dv` 显示 `Signature=adhoc` 和 `Sealed Resources`）。之后用 `ditto -c -k --keepParent Illustash.app Illustash_X.Y.Z_aarch64.zip` 压成 zip 发布，#7、#192 和 0.1.0 的虚拟机验收用的都是这样补签过的 zip。应用内更新（#279）用的包另外打：`python3 tools/updater_release.py pack-mac Illustash.app Illustash_X.Y.Z_aarch64.app.tar.gz`（第一层是 `Illustash.app/`，不带扩展属性），和 zip 来自同一个补签过的 `Illustash.app`。`--ort-dylib`、`--ort-licenses` 取自 PyPI 上 `onnxruntime` 1.30.0 的 macOS arm64 wheel，取法和哈希见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)「macOS 的 CPU ONNX Runtime」。
   - **Windows x64**：在 Windows 验收机上检出发布提交，从本机经 SSH 发起，外面包 `collab_run.py run --slot windows --`：`python packaging/build.py --research --openblas-site DIR --ort-dylib FILE --ort-licenses DIR --directml DIR --output NEW_PATH --desktop-compute packaging/desktop-compute`，再在 `desktop/` 运行 `cargo tauri build --bundles nsis --config ../packaging/tauri-full.json`。只发 NSIS 安装包 `Illustash_X.Y.Z_x64-setup.exe`（当前用户安装），不发 MSI；应用内更新下载的也是这个文件。ONNX Runtime（带 DirectML）的构建办法和 `--directml` 取哪个目录见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)。
5. **两个平台验收**：用要发布的那个文件验收，只用合成数据。
   - **macOS** 在 Tart 虚拟机里做（做法见 [macOS 实机测试](LOCAL_CHECKS.md#macos-实机测试虚拟机)，经 `collab_run.py run --slot vm` 占 vm 槽）：新建标准用户，用虚拟机里的 Safari 下载 zip（带上隔离标记），装进「应用程序」，按 [装不上、打不开怎么办](INSTALL.md#macos) 放行；之后核对关于页的版本是 X.Y.Z、构建号是发布提交的短哈希且不带 `-dirty`，下载模型，建合成图库，同步并识别。标准用户往 `/Applications` 里放东西、在「隐私与安全性」里点「仍要打开」，系统都要输入密码；代理会话不往系统密码框里输密码，所以 0.1.0 是这样做的（去隔离标记这一步用户 2026-11-14 同意）：装进标准用户自己的 `~/Applications`，确认打开时提示「未打开」、「隐私与安全性」里出现「仍要打开」按钮，然后用 `xattr -dr com.apple.quarantine` 去掉隔离标记再打开，验收记录里写明放行这一步没有走。仓库公开前，模型 Release `wd-coreml-v1` 不登录访问返回 404，macOS 上 WD 加速文件的下载会显示「github.com: HTTP 404」，原始模型照常下载、识别照常进行；这一项在仓库公开后按 #190 的清单补验。
   - **Windows** 在 Windows 验收机上做：装 NSIS 包，同样核对关于页、下载模型、建合成图库、同步并识别。验收机装着开发工具链，不是干净系统；首发只在验收机上验收，干净系统上的安装和 SmartScreen 拦截等内测反馈（2026-10-20 用户定）。
   - 从第二次发布起，两边都在装了上一个 Release 的环境里覆盖安装，确认 [更新到新版本之后](INSTALL.md#更新到新版本之后) 写的仍然成立（「下载并重启」要等 Release 公开后才查得到，在第 10 步验）；升级图库格式的版本，再用上一个版本建的合成图库走一遍界面里的升级（#193）。
   - 验收发现问题时，修复照常走 PR；还没打 tag 的版本号不用改，发布提交换成修复合入后的 main 提交，从第 3 步重来。
6. **SHA-256 与更新签名**：Windows 安装包取回本机后，验收机上 `certutil -hashfile FILE SHA256` 的结果要与本机 `shasum -a 256 FILE` 一致。
   - **更新签名**（#279，用户在自己的 Mac 上做，私钥不离开这台机器）：对 macOS 的 `.app.tar.gz` 和 Windows 的 `-setup.exe` 各签一次，`cargo tauri signer sign -f ~/.tauri/illustash-updater.key --app-version X.Y.Z <文件>`，输入私钥密码，在文件旁边写出 `<文件>.sig`。`--app-version` 不能省：应用要求签名绑定版本号（`requireSignedVersion`），没绑定或绑定的版本不对都不装。签名之后不要再改文件名，签名里记着它。
   - **latest.json**：`python3 tools/updater_release.py manifest --mac Illustash_X.Y.Z_aarch64.app.tar.gz --windows Illustash_X.Y.Z_x64-setup.exe --output latest.json`。它读两个 `.sig`，核对签名用的是 `desktop/tauri.conf.json` 里那把公钥对应的私钥、绑定的是 `application.json` 的版本、签的是这个文件名，任何一条不对就停下；下载地址按版本号拼成 `https://github.com/shio-vinyl/illustash/releases/download/vX.Y.Z/<文件名>`。`--notes` 可以写一句说明，应用不显示它，完整说明照旧写在 Release 正文。
   - 在安装包所在目录运行 `shasum -a 256 <zip> <app.tar.gz> <exe> > SHA256SUMS.txt`，文件里只有文件名，不带路径。
7. **打 tag**（用户授权后）：`git tag -a vX.Y.Z -m "Illustash X.Y.Z" <发布提交>`，再 `git push origin vX.Y.Z`。tag 名必须是 `v` 加 `application.json` 里的版本号原样，可用 `git show vX.Y.Z:native/terms/application.json` 核对。
8. **建 GitHub Release**（用户授权后）：先建草稿，`gh release create vX.Y.Z --verify-tag --draft --title "Illustash X.Y.Z" --notes-file <说明文件> <zip> <app.tar.gz> <exe> latest.json SHA256SUMS.txt`。`.sig` 不用上传，签名已经写在 latest.json 里。`--verify-tag` 让 gh 在远端没有这个 tag 时停下，而不是在默认分支上自己建一个。带后缀的版本加 `--prerelease`；不带后缀的版本不要加，否则正式版用户收不到（检查更新也认 GitHub 的 prerelease 标记）。草稿对外不可见，检查更新也看不到；在网页上核对正文和三个文件、下载一个核对哈希之后，再经用户同意 `gh release edit vX.Y.Z --draft=false` 公开；不带后缀的版本公开时加 `--latest`（`gh release edit vX.Y.Z --draft=false --latest`），把它标成仓库的 Latest。现在标成 Latest 的是模型 Release `wd-coreml-v1`，GitHub 按发布时间自动选，模型 Release 以后再发也可能抢走这个标记，所以每次发正式版都显式标一次，公开后用 `gh release view --json tagName --jq .tagName`（不带 tag 就是 Latest）核对是 `vX.Y.Z`。检查更新和「下载并重启」都不看这个标记（第 10 步），它只影响 Release 页面和 `releases/latest` 这个链接。公开后，打开了自动检查的用户下次启动（或开着满 24 小时）就会看到提示。
9. **更新说明**（Release 正文，说明文件放在仓库外，不提交）：
   - 内容来源是这期间合入 main 的 PR：`git log --first-parent --merges --format=%s <上一个tag>..<发布提交>` 列出每个「Merge pull request #N」，标题用 `gh pr list --state merged --base main --limit 500 --json number,title` 取（默认只列 30 个，要加 `--limit`）。按用户看得到的变化整理，不照抄标题；只涉及内部重构、测试、文档的不写。首个公开测试版没有上一个 tag，写这一版能做什么、已知限制和安装办法。
   - 每个 Release 都写：可以在应用里「下载并重启」，macOS 上这样更新不用重新放行、也不用重新允许照片（#279）；自己下载覆盖安装的话，macOS 更新后有两步（2026-10-18 用户定，#192）：第一次打开新版本要在「系统设置 › 隐私与安全性」里重新放行一次，用 iCloud 照片作来源的第一次同步时要重新允许访问照片，原因是临时签名每个版本都不同，链接 [更新到新版本之后](INSTALL.md#更新到新版本之后)；安装包不带模型，第一次使用时下载，链接 [识别模型](INSTALL.md#识别模型)；用 `SHA256SUMS.txt` 核对下载的文件；macOS 版只支持 Apple 芯片的 Mac；请内测用户遇到装不上、打不开时按 [报告问题](INSTALL.md#报告问题) 反馈，Windows 的 SmartScreen 拦截尤其要反馈。
   - 升级图库格式的版本，开头标明「本版会升级图库格式」：打开旧图库时会先询问，升级前自动备份，升级后旧版本打不开；回退办法是装回旧版，再用升级前的备份恢复，备份位置在升级完成后应用会显示一次（规则见 [架构方向](ARCHITECTURE_DIRECTION.md#已有核心库的显式升级边界)）。
10. **检查更新能看到它**（#191，A 档；机制见 [Web 界面](WEB_V2.md#界面语言2026-10-18173) 检查更新一段）：应用不带账号请求 `https://api.github.com/repositories/1407079068/releases?per_page=30`（`1407079068` 是本仓库在 GitHub 上的数字 ID，`gh api repos/shio-vinyl/illustash --jq .id`），只读每项的 `tag_name`、`draft`、`prerelease`，不看正文和资源文件，所以资源文件名不影响检查。一个 Release 要被提示，需要：
    - 已公开，不是草稿；
    - tag 是 `v` 加不带 `+build` 的 SemVer（`v0.2.0`、`v0.2.0-beta.1`），不带 `v`、少一段或带 `+` 的都被忽略；
    - 在接口返回的第一页（30 个）里；
    - 比本机的版本新；带后缀或标了 prerelease 的，只提示给本身是预发布版本的安装。

    「打开下载页」打开的是程序按版本号拼出的 `https://github.com/shio-vinyl/illustash/releases/tag/vX.Y.Z`，不取回复里的链接。公开后用 `gh api 'repositories/1407079068/releases?per_page=30' --jq '.[] | [.tag_name, .draft, .prerelease] | @tsv'` 核对（登录后的 gh 也能看到草稿，应用看不到）。不登录的 `curl -s -w '\nHTTP %{http_code}\n' 'https://api.github.com/repositories/1407079068/releases?per_page=30'` 看到的和应用一样，仓库公开后的第一次发布用它再核对一次，应是版本列表和 `HTTP 200`（#358；仓库还是私有时是 `HTTP 404`）。从第二次发布起，再在虚拟机里用上一个版本在「设置 › 关于」点「检查更新」，确认提示新版本，「打开下载页」打开的是这个 Release。

    **下载并重启**（#279，B 档；机制见 [Web 界面](WEB_V2.md#界面语言2026-10-18173) 检查更新一段）：桌面 App 查到新版本 X.Y.Z 后，用户点「下载并重启」时，外壳下载 `https://github.com/shio-vinyl/illustash/releases/download/vX.Y.Z/latest.json`，再下载其中给本平台的包（`darwin-aarch64`、`windows-x86_64`），包必须在同一个 Release 里。所以要让上一个版本能自动更新到它，这个 Release 里要有 latest.json 和它列出的两个包，文件名与 latest.json 里的一致；只缺 latest.json 时，应用说「这个版本没有提供本机能用的更新包」，退回打开下载页。公开后核对：`curl -sL https://github.com/shio-vinyl/illustash/releases/download/vX.Y.Z/latest.json | python3 -m json.tool` 能读到版本 X.Y.Z 和两个平台；从第二次发布起，在虚拟机和验收机里用上一个版本点「下载并重启」，更新后核对关于页的版本和构建号（第 5 步）。

    **仓库改名或转移**（2026-11-12 用户定，#358）：检查更新的请求不跟随重定向，所以按仓库的数字 ID 查，不按 `账号/仓库名`。改名或转移后 ID 不变，已经发出去的版本照样查得到；「打开下载页」是在浏览器里打开的网页，由 GitHub 的网页跳转接住。按 ID 的地址是 GitHub 把改名或转移过的仓库的旧地址跳转过去的地址（2026-11-12 实测），但没有写进它的 REST 文档。所以 GitHub 明确答复这个地址查不到时（3xx 或 4xx；403、429 是限流，不算），应用会再按 `repos/shio-vinyl/illustash/releases` 查一次：按 ID 的地址哪天不再提供这个列表，只要仓库还叫原来的名字就仍查得到。换地址不是跟随重定向，回复里的跳转地址从不使用；连不上、限流和 5xx 不换地址。已经发出去的版本改不了，由此有两条约束：
    - 改名或转移后，旧的 `shio-vinyl/illustash` 这个位置不能再有仓库（新建和 fork 都算）。一有，GitHub 对旧地址的跳转就失效，已发出版本的「打开下载页」会打开那个新仓库，按名字的那次回退查到的也是它。仓库改名或转移到别的账号、组织时，`shio-vinyl` 账号要留着；账号本身改名的话，GitHub 会放出旧账号名，别人注册后就能在这个位置建同名仓库，这一条就守不住了。
    - 仓库不能删掉重建。重建后 ID 会变，已发出的版本按 ID 查不到；用原名重建时还能靠按名字的回退查到，但以后改名或转移就接不住了。
11. **发布之后**：第一次发布后，改掉 [README](../README.md)「现状」和 [装不上、打不开怎么办](INSTALL.md) 开头「还没有正式发布」的状态句。

#### 更新签名密钥

「下载并重启」（#279）只安装用这把密钥签过的包。它是 Tauri 专用的一对 minisign 密钥，和 Apple、Windows 的代码签名无关。2026-11-14 定：公钥写进仓库，私钥只由用户保管，不进仓库、不交给 CI 和代理会话，也不出现在 Issue、PR 和对话里；签名（第 6 步）由用户在自己的 Mac 上做。

- **生成**（只做一次，用户自己做）：`cargo tauri signer generate -w ~/.tauri/illustash-updater.key`，按提示设一个密码（不要留空）。它写出私钥 `illustash-updater.key` 和公钥 `illustash-updater.key.pub`。
- **公钥进仓库**：把 `.key.pub` 文件的内容（一行 base64）原样写进 `desktop/tauri.conf.json` 的 `plugins.updater.pubkey`，开一个普通代码级的 PR。公钥可以公开。这个 PR 合入之后打出的包才会出现「下载并重启」。现在的公钥 2026-11-14 写入，密钥编号 `3DAAE3E564F03C34`（`tools/updater_release.py manifest` 核对的就是它）。
- **备份**：私钥文件和密码分开放，各至少两处，其中一处离线，例如密码存在密码管理器里，私钥文件存在密码管理器的附件和一个不联网的 U 盘里。私钥本身已用密码加密，单丢文件不会泄露，单丢密码也不会；两样都要才签得了名。
- **丢了私钥或密码**：已经装好的版本只认写在它里面的那把公钥，再也签不出它们肯装的包：这些安装点「下载并重启」都会失败（「更新包的签名不对，没有安装」），自动退回「打开下载页」，手动下载、覆盖安装照常可用。退路是生成一对新密钥，新公钥写进配置发一个版本，在 Release 正文里请所有人手动更新这一次，之后的版本用新密钥签。
- **私钥泄露了**：别人可以签出应用肯装的包，但还要让用户下载到它，也就是要能往本仓库的 Release 里放文件（应用只从这个版本的 Release 下载，走 HTTPS）。处理办法同上：换一对密钥、发版、请大家手动更新一次，并检查仓库的访问权限。
- **核对是不是一对**：第 6 步的 `tools/updater_release.py manifest` 会比较签名和配置里公钥的密钥编号；不一致时停下，不会写出 latest.json。

## 图库包

- `library.json`：包清单、来源与已移除来源记录。
- `source.sqlite`：来源快照，只读，不接收人工写入。
- `library.sqlite`：人工结论、机器证据与统一撤销历史，**不可当缓存删除**。
- `provenance.sqlite`：找回画师的查询缓存与机器结果，不进导出包；`features/`：本图库的 CCIP 特征文件（`full-features.npy`、`full-manifest.csv`）。
- `thumbnails/`：可重建；`backups/`：`--backup` 与升级前的备份。
- `.illustash-owner.lock`：打开它的 Illustash 持有的系统文件锁；`.illustash-checks.json`：会话是否开着、两个库上次完整检查通过的时间、检查发现的损坏（#212，见[架构方向](ARCHITECTURE_DIRECTION.md#完整性检查)）。都由打开图库的会话维护，不要手改。

包外的共享资源（参考库、模型、研究输出）与目录职责见 [图库参考](LIBRARIES.md#2-包外的共享资源)。

## 目录

```text
illustash/
├── crates/               词条核心、平台能力、IQDB 签名、FFT 编译
├── native/terms/         应用宿主：Core 适配、CLI、HTTP、任务编排
├── desktop/              Tauri 桌面外壳：启动服务与 Pixiv 登录窗口
├── web/static/           静态界面
├── packaging/            安装包与便携产物构建、资源白名单
├── tools/                构建、检查与验收脚本（用途见下文「tools/ 目录」）
├── tests/                合成测试、契约夹具与录下的旧参考固定期望
├── docs/                 现行参考、进行中的方案与待办
├── tags/                 标签表（tags_v3.csv）
├── start-web.command     本地 Web 入口（macOS）
├── AGENTS.md             协作流程与文档规范，CLAUDE.md 引入本篇
├── LICENSE               GPL-3.0 全文
└── README.md             对外介绍
```

本机忽略、不进仓库：`models/`（ONNX 模型）、`review/`（共享索引与研究数据）、`reference/`（研究参考材料）、`_scratch/`（可重新生成的产物）、`.venv/`（本机依赖与固定验收缓存）；`.claude/worktrees/` 放开发 worktree。

## tools/ 目录

`tools/` 只放构建、检查与验收脚本，Python 源码不进安装包；安装包里实际带哪些文件以 `packaging/build.py` 的白名单为准（见 [安装交付](BACKEND_TASKS.md#独立安装与可选计算包)）。保留与删除的依据见 #186；旧 Python 索引工具已于 2026-10-08 删除（#53），正式索引计算由原生 `illustash-worker` 完成；研究脚本已全部移出本仓库（2026-10-06、2026-10-07）。

| 脚本 | 用途 |
|---|---|
| `check_all.sh`、`local_acceptance.py` | 本地全量检查与固定路径验收，见 [LOCAL_CHECKS.md](LOCAL_CHECKS.md) |
| `check_architecture.py` | crate 依赖方向检查 |
| `check_frontend.py` | 前端结构检查：`web/static` 的依赖方向、state 声明与写入者、全局监听、对话框、超长行、页面名判断、`app.css` 的写死字面量（#349）；基线 `frontend_baseline.json` 只能缩小（#320），新规则首次登记存量用 `--init-rule` |
| `check_styleguide.py` | 样式清单页（`styleguide/`，只在开发时用、不进发布包，迁移清点不算生产代码）与实现的同步检查：`census.js` 要等于 `styleguide/census.py` 对当前 `app.css` 的输出，样例和条目标题用到的类要在 `app.css` 里有样式；`styleguide/live.js`（调用 `web/static/ui` 的生成函数画样例）导入的名字要在产品代码里导出、要取的外壳 id 要还在、舞台和样例一一对应，组件条目都标明“调用生成”或“手写”（#352） |
| `migration_inventory.py` | 生产迁移边界清点，规格在 `tests/migration_inventory.json` |
| `gen_casefold.py`、`gen_http_unicode.py` | 生成 `crates/core` 的 casefold 与 Unicode 表 |
| `test_native_contracts.sh`、`bench_native_contracts.sh` | 空 PATH 重放固定契约；契约基准，见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md) |
| `bench_recognition.py` | 整应用识别吞吐基准（合成图片，扫描到识别、导入完成），见 [TERMS_PERFORMANCE.md 第 6 节](TERMS_PERFORMANCE.md#6-整应用识别吞吐基准) |
| `verify_read_http.py` | 原生测试用的 HTTP 逐字节对照辅助 |
| `build_provenance_probe.py`、`windows_acceptance.py`、`windows_research_acceptance.py` | 溯源候选构建、Windows 基础版与研究包验收 |
| `build_db.py` | 重建旧格式分析源库；`test_native_library_upgrade` 用它造旧库 |
| `dev_preview.py` | 浏览器走查用的隔离宿主（临时目录、合成图库），`.claude/launch.json` 的 `web-isolated` 调用它，见上文「快速开始」 |
| `version.py` | 版本号的唯一来源是 `native/terms/application.json`（#189）：检查或改写 `tauri.conf.json`、各发布 crate 的 `Cargo.toml` 与 `Cargo.lock` |
| `updater_release.py` | 应用内更新的发布文件（#279）：打 macOS 更新包、生成 `latest.json`，步骤见上文「发布」 |
| `gen_rust_licenses.py` | 生成 `packaging/rust-licenses/` 的第三方 Rust crate 许可（#161），依赖或 `Cargo.lock` 变了要重跑 |
| `build_wd_coreml.py` | 生成 `models.json` 固定的 WD Core ML 执行变体（#375），只在维护者构建模型 Release 时用 |
| `mac_icon.py` | 由 `docs/assets/icon-transparent.svg` 生成 macOS App 图标（#293），只能在 macOS 上运行 |

## 文档地图

现行参考都在 `docs/`；根目录只留 `README.md`、`AGENTS.md`、`CLAUDE.md` 和 `LICENSE`。

| 主题 | 文档 |
|---|---|
| 产品底线与待办 | [BACKLOG.md](BACKLOG.md) |
| 架构与迁移方向 | [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md) |
| 词条核心 | [TERMS.md](TERMS.md)、[TERMS_SCHEMA.md](TERMS_SCHEMA.md)、[TERMS_ARTISTS.md](TERMS_ARTISTS.md)、[TERMS_LABELS.md](TERMS_LABELS.md)、[TERMS_CHARACTERS.md](TERMS_CHARACTERS.md)、[TERMS_NAMES.md](TERMS_NAMES.md) |
| 图库与界面 | [LIBRARIES.md](LIBRARIES.md)、[WEB_V2.md](WEB_V2.md)、[WEB_UI_MAP.md](WEB_UI_MAP.md)、[DATA_MODEL.md](DATA_MODEL.md) |
| 识别与证据 | [RESULT_PACKAGES.md](RESULT_PACKAGES.md)、[EVIDENCE_PACKAGES.md](EVIDENCE_PACKAGES.md) |
| 溯源 | [PROVENANCE.md](PROVENANCE.md) |
| 安装与平台 | [BACKEND_TASKS.md](BACKEND_TASKS.md) |
| 验收与性能 | [LOCAL_CHECKS.md](LOCAL_CHECKS.md)、[TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md) |
| 工具与许可 | [tools/ 目录](#tools-目录)、[THIRD_PARTY.md](THIRD_PARTY.md) |

分类与维护规则见 [AGENTS.md](../AGENTS.md) 的「文档维护规范」：仓库只留现行参考、进行中的方案、待办与约束三种文档，任务与流水放 GitHub。

## 开发与验收

- 工具链：Python 3.12、Node 22、Rust stable（含 rustfmt/clippy）；Python 依赖与历史构建缓存放在被忽略的 `.venv/check-all/`。
- 开发期和合入其他分支的 PR 按改动跑定向检查；目标为 main 的非纯 Markdown PR 先完成其他平台检查，再在已提交的干净候选上跑一次本机完整入口 `bash tools/check_all.sh`（对齐 `.github/workflows/ci.yml`）。固定验收区用 `python3.12 tools/local_acceptance.py run <候选SHA>` 串行执行并写签名记录，`result <候选SHA>` 取结果。
- 允许的跳过只有 CI 同样允许的平台限制和真实图库数据缺失；不访问真实图库补齐数据。
- 只改 Markdown 的 PR 不跑全量，只检查相对链接。
- GitHub Actions 对目标为 main 的同库非草稿 PR 自动运行 `ci.yml`，作为额外保障，不代替本机全量；规则见 AGENTS.md 2026-11-12 一条，做法见 [本地验收](LOCAL_CHECKS.md#github-actionspr-自动检查)。
- 常用入口：核心测试 `cargo test --locked --manifest-path native/terms/Cargo.toml`，Python 合成套件 `python -m tests.run_offline -v -b`（用装了依赖的 3.12 解释器）。
- `native/terms` 的集成测试合成三个测试二进制（#310）：`tests/it/`（其余各文件是 `it/main.rs` 的子模块，只跑一个用 `--test it entry::`）、`tests/contracts.rs`（检查脚本和计时按 `--test contracts` 单独构建）、`tests/baseline.rs`（进程级的 SQLite 设置要最先生效）。新的集成测试放进 `tests/it/` 并在 `main.rs` 登记；每个测试第一行取 `crate::shared()`，要改进程环境变量或当前目录的取 `crate::exclusive()`，`it` 里有测试检查这一点。在 `tests/` 顶层另建文件也能跑，但会多编、多链接一次整个库。
- 开发时也可以用 `cargo nextest run` 跑这些测试（需自行安装）：它每个测试起一个进程、跨二进制并行，`it` 的环境锁和 `baseline` 的进程级设置都照常成立；它不减少编译，只缩短运行。`integrity`、`rescue` 在负载高时本来就不稳定（#395），并行更多时更容易碰到。全量和 CI 仍用 `cargo test`。

细节与复用口径见 [本地全量验收](LOCAL_CHECKS.md)。

## 协作

任务单、进度、审核与验收都放 GitHub（`shio-vinyl/illustash`）：任务在 Issue，进度与验收在草稿 PR 描述，交接在最新一条评论，需要用户决定的事在 Issue 里提问并打「待决定」标签。仓库里只放现在为真的东西，规则见 [AGENTS.md](../AGENTS.md)。


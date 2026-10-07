# 本地全量验收

现行参考，2026-10-03。本文补充 [AGENTS.md](../AGENTS.md) 的本地验收约定；CI 覆盖及允许跳过仍以 `.github/workflows/ci.yml`、`tools/check_all.sh` 为准。

## 开发检查与完整入口

开发 worktree 先跑新改测试和相关定向检查。`tools/check_all.sh` 先验证测试文件语法及验收工具合成测试，再执行 crates/core 与 crates/platform 独立 fmt/clippy/test/locked build、宿主 fmt/clippy、JS、桌面 clippy/test，随后执行原有 Rust、release 构建、Python discover 和空 PATH 契约重放。覆盖、失败即退出、跳过白名单不变。构建前只运行固定的测试语法、验收工具合成测试等纯轻量项。任意指定的定向测试使用 `CHECK_ALL_TARGETED_TESTS='tests.test_native_http.TestCase'`（使用完整模块路径，在仓库根执行，以实际类名为准），在本轮候选 release 核心、测试读宿主及 IQDB 准备完成并设置二进制路径之后、Python discover 之前运行；输出候选 head，不提前读取旧 target。旧变量 `CHECK_ALL_PREFLIGHT_TESTS` 非空时明确退出 2，不静默改变其含义。额外定向结果不能代替全量，也不缩小 discover 的覆盖。

当前候选的 `read-http`、`recognition-compute`、`index-compute`、`research-compute`、`relation-probe`、`provenance-compute` examples 在本轮 release 构建中显式准备，再导出各自 `ILLUSTASH_*_BIN`；研究/索引安装适配器同样显式指向本轮 `illustash-worker`。本地与 CI 使用相同接线，缺产物不能靠测试默认跳过形成通过。模型的专项授权及平台证据仍单列。

直接执行 `tools/check_all.sh` 仍要求已提交的干净候选，运行期间 head/已跟踪文件保持不变。验收工具合成测试的前置与全量都使用 `unittest -b` 捕获模式，文本捕获器不提供二进制输出流时仍保留原始完整日志及其摘要。脚本独立执行不会生成可复用签名记录。缓存速度没有固定承诺。

2026-10-03 用户批准的 PR #131 / #136 / #141 是限定合批例外：各项保留独立 review 和受影响平台专项，整合保留原提交，最终整合 head 一次完整入口通过后整批 merge；原 PR 的历史或定向证据单列，不冒称逐项新 head 全量。其余任务仍按 AGENTS.md 原合并门槛执行，不能因复用或合批省略实际 GUI / 安装验收。

## 固定验收窗口

固定路径为 Git 公共目录所在仓库的 `.venv/local-acceptance/worktree`，由 `tools/local_acceptance.py` 建立 detached worktree。它与主工作区及开发 worktree 的源码、target 分开；只在固定路径现场构建，不复制其他目录的 Tauri target。开发 worktree 的缓存可按原规则清理，固定验收 worktree、它的 target / `.venv/check-all` 以及外层记录不能随普通任务收尾删除。

开发方先提交候选、做定向检查，在 PR 交接完整 SHA 和所需资源。父会话安排窗口后，从本仓库任一 worktree 执行：

```bash
python3.12 tools/local_acceptance.py run <已提交候选SHA>
python3.12 tools/local_acceptance.py result <候选SHA>
```

工具不 fetch、不 push、不合并。它自动创建或移动自己登记的固定 worktree；现有未知目录、分支模式、未提交或未跟踪内容均拒绝，不覆盖。两个命令都以非阻塞进程锁串行操作，第二个占用者直接失败。运行子进程继承锁描述符，即使控制器退出，仍在运行的自有检查进程保有锁；正常结束即释放，无持久占用或手工解锁指令。中断不产生成功记录，不杀其他会话。不得绕开此入口直接在固定路径切 head 或写 target。固定区只能串行验收一个已提交候选，没有排队、合批或调度服务。

`result` 会在持锁期间选择该候选并验证已有结果；不运行构建或测试。无匹配结果时退出非零。`run` 先尝试复用，无匹配则从头执行唯一完整命令 `bash tools/check_all.sh`，输出同步写入外层 `check-all.log`。完整命令退出 0、输入和 head 保持不变、工具链和环境校验通过之后才写 `result.json`。失败或中断移除旧成功记录，保留失败日志供调查，不能称为通过。

## 复用口径与失效

默认指纹包含所有已跟踪文件的路径、Git blob 和模式：源码、测试、脚本、锁文件、配置、所有现行文档和生成资源都不例外。只允许 `docs/acceptance/local-summary.md` 这一个普通非可执行文件用作不被产品、构建或测试读取的验收叙述；不提交原始运行日志。该文件不是测试输入，新增读取、include、打包或按通配符消费它的代码必须同时取消豁免，并重新全量。其他文档的说明收尾仍须重跑。工具/规则/指纹策略变更也失效，不能简单排除全部 Markdown。

记录保留原实测 commit、tree、完整命令、exit 0、开始/结束时间、完整日志摘要及有效输入。候选必须是已测提交的线性后代（不含 merge），每个提交只能改变上述叙述文件；即使其他提交改回相同代码，也不在复用范围。候选 tree 和已测 tree 分别展示。原生构建嵌入 Git SHA：复用的是原提交完整运行的验收事实，原产物保留原 SHA；不声称当前候选重新构建或其产物字节相同，不把当前 SHA 标到原产物上。需要候选 SHA 的实际发行构建另行构建并验收。

环境指纹包含同一固定路径、机器名、OS/架构、全部传入环境变量的摘要、Python/Node/Bash/Rust 工具链版本和文件摘要、实际 rustup 工具链、Cargo/Git 配置、macOS SDK/实际 clang 和明确区分的 Command Line Tools 安装身份或完整 Xcode 版本（探针失败不忽略），以及已安装 Python 依赖文件和模式。环境变量明文不会写入记录；`PWD` 固定化，`_`/`OLDPWD`/`SHLVL` 等壳层状态和被完整脚本明确 unset 的 `CARGO_TARGET_DIR` 移除后传给检查。首次安装 Python 依赖按完整脚本要求执行，记录安装后的实际依赖指纹；其余工具链/环境在运行前后必须一致。

被忽略的 `packaging/desktop-resources` 内容纳入指纹。桌面构建输出仅窄识别 `desktop/dist/splash.js` 及 `desktop/gen/schemas` 下的 `acl-manifests.json`、`capabilities.json`、`desktop-schema.json` 和 `macOS-schema.json`。`splash.js` 必须与已跟踪的普通文件 `web/static/splash.js` 逐字节一致；Tauri 文件必须为普通 JSON object 文件，首次由完整检查生成后记录各自内容和模式，签名记录同时绑定已跟踪的 build.rs、capabilities/permissions、Cargo.lock 等源输入及工具链。复用时这些已测输出也须逐项匹配，删除/篡改使复用失效；完整命令之后缺少要求输出不得记录成功。输入指纹不因这些具体输出从无到有而变动，产物证据与输入分别校验。旧候选的 splash 不一致时拒绝复用，但可由新的完整检查重新复制源并校验后登记；不泛化豁免整个 dist 或 gen，未知文件/符号链接仍拒绝。ONNX Runtime 1.30 导入会生成根目录 `:memory:.ses`，只将这一普通文件的时间戳/UUID 固定格式纳入同样的签名生成输出证据，不豁免任意 `:memory:*`；未知名称或格式拒绝，文件变化使复用失效。记录格式为 v3，旧 v1/v2 记录失效并需完整执行。未知忽略输入（例如外部 models/reference）直接拒绝，不读取内容；各已登记 crate（包括 crates/platform）的 target、工具 `.venv` 缓存、Python 字节码和 `.DS_Store` 属于可再生输出。固定区不得额外放置外部输入；若引入新的被忽略构建输入，应先修改策略和合成测试。历史产出者仍由完整脚本按 CI 提交号构建。

本机私有权限密钥对记录作 HMAC 校验，完整日志也校验摘要；缺字段、失败、定向命令、签名/日志篡改、输入或环境不匹配均拒绝。该记录用于防止误改/手工修改记录后的复用；拥有本机密钥和工具源码写权限的人不是安全隔离边界。不能把记录移到另一机器或平台冒称覆盖。密钥丢失则旧记录无效，从头完整运行。

PR 应明确区分“当前 head 上完整执行退出 0”和“当前候选仅验收叙述变化，复用已测 commit 的全量事实”，列出双方 commit/tree、固定路径、机器/平台/工具链、结果和跳过理由；其他平台与尚未做的 UI 验收单列。缓存收益只引用同一口径的实测时间；没有实测就不写提速数字。

# 本地全量验收

现行参考，2026-10-15。本文补充 [AGENTS.md](../AGENTS.md) 的本地验收约定；CI 覆盖及允许跳过仍以 `.github/workflows/ci.yml`、`tools/check_all.sh` 为准。

## 开发检查与完整入口

开发 worktree 先跑新改测试和相关定向检查。`tools/check_all.sh` 先验证测试文件语法及验收工具合成测试，再执行 crates/core 与 crates/platform 独立 fmt/clippy/test/locked build、宿主 fmt/clippy、JS、桌面 clippy/test，随后执行原有 Rust、release 构建、屏蔽退役参考目录导入的 Python discover 和空 PATH 契约重放。Python 全套由 `tests.run_fixed_suite` 执行，调度和参数见下文；覆盖、失败即退出、跳过白名单不变。导入源位于 `tests/reference/`、`web/` 或 #53 已退役的旧索引工具文件时立即拒绝，任何被捕获的导入尝试也使检查失败。构建前只运行固定的测试语法、验收工具合成测试等纯轻量项。任意指定的定向测试使用 `CHECK_ALL_TARGETED_TESTS='tests.test_native_http.TestCase'`（使用完整模块路径，在仓库根执行，以实际类名为准），在本轮候选 release 核心、测试读宿主及 IQDB 准备完成并设置二进制路径之后、Python discover 之前运行；输出候选 head，不提前读取旧 target。旧变量 `CHECK_ALL_PREFLIGHT_TESTS` 非空时明确退出 2，不静默改变其含义。额外定向结果不能代替全量，也不缩小 discover 的覆盖。

当前候选的 `read-http`、`recognition-compute`、`index-compute`、`research-compute`、`relation-probe`、`provenance-compute` examples 在本轮 release 构建中显式准备，再导出各自 `ILLUSTASH_*_BIN`；研究/索引安装适配器同样显式指向本轮 `illustash-worker`。本地与 CI 使用相同接线，缺产物不能靠测试默认跳过形成通过。模型的专项授权及平台证据仍单列。

直接执行 `tools/check_all.sh` 仍要求已提交的干净候选，运行期间 head/已跟踪文件保持不变。验收工具合成测试的前置与全量都使用 `unittest -b` 捕获模式，文本捕获器不提供二进制输出流时仍保留原始完整日志及其摘要。脚本独立执行不会生成可复用签名记录。缓存速度没有固定承诺。

2026-10-03 用户批准的 PR #131 / #136 / #141 是限定合批例外：各项保留独立 review 和受影响平台专项，整合保留原提交，最终整合 head 一次完整入口通过后整批 merge；原 PR 的历史或定向证据单列，不冒称逐项新 head 全量。其余任务仍按 AGENTS.md 原合并门槛执行，不能因复用或合批省略实际 GUI / 安装验收。

## 并行运行与耗时口径

`tests/run_fixed_suite.py` 默认以 CPU 核数为进程上限，按测试类分组动态领取，同一类在一个子进程内运行；`--jobs N` 指定正整数上限，`--jobs 1` 使用原串行 discover。并行模式仅接受 `-v`、`-b`，筛选等其他 unittest 参数需配合 `--jobs 1`。每组输出整段回放，父进程检查发现的测试 ID 是否漏跑或出现额外项，子进程崩溃、测试失败及退役导入尝试均使入口失败。子进程以 `-c` 启动，先把仓库根插到 `sys.path` 最前再运行 `tests.run_fixed_suite`，并沿用父进程的 `-I`、`-X utf8`、`-W` 等解释器选项，因此不依赖解释器把 cwd 放进 `sys.path`；带 `._pth` 的嵌入式 Python（Windows 验收机）与 `-I` 一样不加 cwd、忽略 `PYTHONPATH`，父进程须同样自行插入仓库根后用 `runpy` 运行，`tests/test_run_fixed_suite.py` 以 `-I` 覆盖这一情形。单独调用运行器前仍须准备对应候选二进制和环境；改变进程数不构成完整验收。

`SERIAL` 是运行器内的测试类名单：并行池结束后，名单里的类逐个单独运行，每个用例仍只执行一次，不自动重试。2026-10-13，[PR #134](https://github.com/shio-vinyl/illustash/pull/134) 修复找回画师终态与忙状态不一致、PID 文件未写完整及研究调用计数混入其他任务的问题后，名单为空（`SERIAL=frozenset()`），全部正常发现的测试类进入并行池。临时加入名单时，在对应 Issue 记录失败现象和移除条件；修复后验证并行运行并移除，不把串行结果当作竞态已修复。

2026-10-13 的合成数据实测：Apple M2 8 核、macOS 26.6.2 arm64，Python 3.12.12、Node 22.23.2、Rust/Cargo 1.94.1。最终已测 head `5ad38c9`（包含 #131 全部优化）单次完整检查为 **569 秒（9 分 29 秒）**，其中 Python 全套 8 进程为 122 秒（638 项、611 通过、27 白名单跳过），Rust 重放为 220 秒；依赖缓存已存在。此前 `0748ced` 单次完整检查为 903 秒，包含 dev 依赖优化级别变化后的约 190 秒一次性重编。验收详情分别见 [PR #134](https://github.com/shio-vinyl/illustash/pull/134) 和 [PR #131](https://github.com/shio-vinyl/illustash/pull/131)；这两轮未覆盖 Windows，不能把耗时外推到其他机器、平台或冷缓存，也不表示后续 main 已重新完整运行。

全量耗时包含依赖准备、fmt/clippy、测试编译、Rust/JS/Python 执行、release 构建和空 PATH 重放；Python 汇总及 Rust 重放各只是其中一部分。比较时写明已测 head、平台、进程数、数据口径和缓存是否已存在，分别列出编译与执行时间。当前 Rust 重放缓存每个场景的路径替换规则并预筛无目标字符串的单元格；`native/terms/Cargo.toml` 的 dev profile 优化第三方依赖，自有 core/platform 和修改过的 tiny_http 保持未优化。依赖重编的单次总时长、测试执行时间与固定验收缓存复用是不同口径，不能合成一个提速数字。

## 本机资源：排队、计时与后台优先级

规则见 [AGENTS.md](../AGENTS.md) 2026-10-12、2026-10-18、2026-10-19 的条目，这里写做法。工具是 agent-collab skill 的 `scripts/collab_run.py`（路径按所在 harness 的 skill 安装位置），槽位是本机文件锁，没有常驻进程。

| 用途 | 命令 | 行为 |
|---|---|---|
| 全量、`local_acceptance.py run`、单独的 release 构建 | `collab_run.py run -- <命令>` | 占 heavy 槽，被占时排队 |
| 计时、性能测量 | `collab_run.py timing -- <命令>` | 同时占 heavy 和 vm（要么都拿到，要么都不占），再等整机 CPU 连续 30 秒占用不超过 10% 才开始；15 分钟内等不到就放开槽位、退出 75，并列出最占 CPU 的进程。`--idle-cpu`、`--idle-for`、`--idle-timeout` 可调，调过的值写进 PR 的测量口径 |
| 虚拟机 | `collab_run.py run --slot vm -- tart run …` | 见下文「macOS 实机测试」 |
| Windows 远端构建与验收 | `collab_run.py run --slot windows -- …` | 与本机槽位互不阻塞 |
| 开发期的编译和定向测试 | `collab_run.py bg -- <命令>` | 不排队，直接以后台优先级运行 |

`bg` 在 macOS 上用 `taskpolicy -b`：进程只跑能效核，磁盘 I/O 被节流，cargo、rustc、测试进程和 Python 子进程都继承（`ps -o pri=` 显示 4，普通进程是 31）。其他系统退回 `nice -n 10`，没设 `CARGO_BUILD_JOBS` 时设为核数的一半。在 `run` / `timing` 里面调用 `bg` 时保持正常优先级，所以全量和计时不会被降级。对时间敏感的定向测试在后台优先级下失败时，先在 `run` 里重跑排除负载，再下结论（#219）。

**拦截**：Claude Code 和 Codex 都接了 `PreToolUse` 钩子，按本机的 `~/.config/agent-collab/run.json` 拦下没按规定运行的命令，并给出改写后的完整命令。2026-10-19 起的范围：

- heavy：`tools/check_all.sh`、`tools/local_acceptance.py run`、`cargo build --release` / `-r`、`cargo test --release`、`cargo tauri build`、`python -m tests.run_fixed_suite`
- 计时：`tools/bench_recognition.py`（新加的计时脚本也应加进来）
- windows：`tools/windows_acceptance.py`、`tools/windows_research_acceptance.py`
- 后台：`cargo build`、`cargo test`、`cargo clippy`、`cargo check`、`cargo nextest`、`python -m unittest`；直接写 `taskpolicy -b …` 也放行，经 SSH 在别的机器上跑的不管

钩子只看代理直接发起的命令，脚本内部调用的（例如 `check_all.sh` 里的 cargo）不受影响。`cargo run` 不在范围内：常驻的宿主进程不应长期占着槽位或被降级。

**运行历史**：每次 `run`、`timing`、`bg` 结束（包括排不上、等不到空闲而放弃的）都追加一行到状态目录的 `history.jsonl`：模式、槽位、会话、仓库、命令、排队、等空闲和执行的秒数、退出码、开始时的负载，以及当时别的槽位是否被占。`collab_run.py history` 按槽位汇总最近 7 天（次数、失败、放弃、排队和执行时长的中位数与最大值），`--raw N` 列出最近 N 次。讨论排队代价、全量耗时时以这里的数据为准，引用时写明时间范围。

**回退**：钩子环境里设 `COLLAB_RUN_GUARD=0` 关闭全部拦截；只想取消某一类，从 `run.json` 删掉对应规则。`bg` 不改变任何持久状态，不用就是回退。

## 修改测试的导航

业务契约和固定期望的比较规则见 [架构参考](ARCHITECTURE_DIRECTION.md)；本节说明测试分工、依赖和修改方法，测试入口与合并门槛仍按上文及 AGENTS.md。

| 要验证的行为 | 优先阅读或修改的位置 | 依赖与范围 |
|---|---|---|
| 事务、证据、人工历史、投影和业务规则 | `crates/core/src/` 的测试模块、`crates/core/tests/` | 直接调用核心，多数用内存 SQLite；适合规则的边界组合 |
| 文件、凭据、进程、系统数学库等平台能力 | `crates/platform/src/` 的测试模块、`crates/platform/tests/` | 以真实平台边界为准；其他平台的证据单列 |
| 宿主任务状态、取消、切库与提交顺序 | `native/terms/src/` 的测试模块；`tests/test_native_lifecycle.py`、`test_native_sync.py`、`test_native_research.py`、`test_native_provenance.py` 等 | 模块内测试可控制实际任务阶段；HTTP 测试验证调用方看到的状态和结果 |
| 旧行为与旧图库兼容 | `native/terms/tests/replay.rs`、`contracts.rs`；`tests/fixed_expectations.py`、`test_native_library_upgrade.py` | 使用提交的录制或历史合成包；期望独立于当前候选输出 |
| 第一个公开测试版的对照基准（#201） | `native/terms/tests/baseline.rs`、`baseline_http.rs`，比较规则在 `tests/common/baseline.rs`；Python 侧在 `tests/fixed_expectations.py` 末尾 | 由 main（核心第 7 版）录制，核心 v8 起取代上一行的旧录制，见下文 |
| 数值、图像及计算发布 | `tests/test_native_*_compute.py`、`test_native_*_production.py` | 显式指定候选 examples/worker；固定数值由 `*_expectations.py` 读取，Windows 差异另有覆盖文件 |
| 界面、资源装配与检查工具 | `tests/*.test.cjs`、`test_pixiv_poll.js`、`test_packaging*.py`、`test_local_acceptance.py`、`test_architecture.py` | JS 与合成工具测试不等于 GUI 或完整安装验收 |

真实模型专项为 `tests/platform_*_models.py`，独立于默认 `test_*.py` discover；模型、runtime 和候选必须显式提供。不能把合成计算或资源装配通过写成真实模型、桌面 GUI 或另一平台通过。

### 新增与收拢覆盖

新增用例先写清可触发的条件和可观察的错误结果，再选择能覆盖它的最低一层。规则的边界组合优先直接调用核心；HTTP、CLI 或 worker 层保留请求接线、授权、发布及生命周期的代表性场景。同一规则跨层出现时，应能说明各层独有的失败方式。收拢前对照输入、断言和状态边界；相似名称、相同路由或同样出现 SQLite 不足以证明重复。

普通新用例优先检查受影响的返回值、行和业务不变量；拒绝写入、事务回滚、切库隔离等本身要求其他状态不变的场景，才做相应范围的完整状态比较。已有固定录制的比较范围按既定契约维护，不能借性能优化直接删减。

### 固定期望与共享夹具

`tests/fixtures/reference_recordings/` 按测试 ID 和调用顺序取值，`native/terms/tests/fixtures/replay/` 按模块重放核心调用；改测试模块、类名、方法名或取值顺序之前先查读取方。`tests/image_expectations.py`、`index_expectations.py` 等是冻结答案的读取器，旧模块名出现在这些文件里不表示旧业务实现仍在执行。比较失败先区分输入、路径/时钟归一化、平台差异和产品行为变化；原生输出不得刷新旧答案。产品决定确需改变期望时，按架构参考的规则在对应 PR 说明。

**新对照基准（#201）**：核心 v8 删掉旧兼容表后，上面三类旧录制（24 组核心调用重放、4 组 HTTP 固定契约、Python 固定期望里的整库摘要）都比较全部表、schema 和隐式 rowid，会一起失效。2026-10-19 起用当时的 main 在同样的场景上另录了一份新基准，放在 `native/terms/tests/fixtures/baseline/` 和 `tests/fixtures/baseline_states/`：接口回复按字节比较；每次调用之后比较除退役旧表以外的全部表内容，不比 schema 和隐式 rowid；只服务旧表的调用（`sync-*`、`verify-*`、`cutover-*` 和未切换完的库上的调用）不进新基准，它们留下的库作为检查点，在下一个保留的调用之前载入，处于录制时核心版本的检查点先升级。比较规则写在 `native/terms/tests/common/baseline.rs` 开头，两边共用；Python 侧照同样的规则，在录下的状态与实时状态比较时一并比对。比较规则由用户 2026-10-19 确认（#201）；核心 v8 第一次改表结构之前，新旧两套并存，都要通过。录制只在 main 仍与旧录制一致时进行：`ILLUSTASH_RECORD_BASELINE=1` 下运行 `cargo test --test replay record_baseline -- --ignored`、`cargo test --test contracts`，以及 Python 固定期望的相关模块；录制过程中旧录制照常核对，不一致就不录。

HTTP 测试共用 `tests/test_read_http.py` 的端口、等待和进程清理工具；多种原生测试借用 `tests/test_native_lifecycle.py` 的建包与宿主方法，worker 启动与进程存在性工具在 `tests/native_worker_fixture.py`。测试对宿主的 HTTP 请求统一经 `tools/verify_read_http.py` 的 `fetch`：每个请求一条短连接，读完响应后以 RST 关闭，本机不留 TIME_WAIT。逐个正常关闭时，并行全套约 1.5 万个连接会在 Windows 上耗尽默认的 16,384 个临时端口（#140）。新增大量请求或轮询时沿用 `fetch`，不要另起逐次关闭的客户端；只有验证连接本身行为（半包、卡住、慢读）的测试才自建 socket。修改这些共享方法须检查实际使用它们的测试类，别只看定义文件。提取 helper 时避免把带 `test_*` 方法的 `TestCase` 作为模块级别名留给 discover；`__module__` 还可能参与固定录制的定位，移动前须核对。

### 慢测试与时序失败

排查时分别计入构建、建库/复制、启动进程、执行被测操作、扫描与比较期望、等待/清理；同时记录调用次数、进程次数和参与比较的数据量。录制重放的一个 Rust 测试函数包含多次调用，每次还会扫描数据库；测试项数不能代表工作量。先消除重复执行和比较器内部的重复计算，再根据实测决定并行粒度。共享 Cargo 产物的任务可能争锁，录制模块内部也已有并行线程；增加外层并行前须核对这些依赖及机器负载。

等待状态应以完整结果为条件：文件存在不保证 PID 已写完，终态可见也需符合下一次操作的接纳契约。超时用例单独设置短超时；其他用例的等待上限应容纳正常负载。取消或提交边界优先用已存在的阶段闸门、进度回调或显式握手定位；自动重试和一律加长 sleep 会掩盖竞态。失败信息应带最后状态、子进程日志或不一致的表/字段，避免只剩一个通用等待超时。

调整 `tools/check_all.sh` 的结构时检查 `tests/test_local_acceptance.py` 与 `test_packaging_resources.py`：它们目前还按脚本文本定位构建、环境导出和 discover 顺序。维护时保留候选产物先准备再消费、漏跑失败及跳过白名单等行为；新增检查优先验证行为，避免继续绑定命令的排版与拼写。全量检查前置的 `test_local_acceptance.py` 也会被后续 discover 再执行；任何去重须同时保留快速失败入口及最终覆盖。

## 固定验收窗口

固定路径为 Git 公共目录所在仓库的 `.venv/local-acceptance/worktree`，由 `tools/local_acceptance.py` 建立 detached worktree。它与主工作区及开发 worktree 的源码、target 分开；只在固定路径现场构建，不复制其他目录的 Tauri target。开发 worktree 的缓存可按原规则清理。固定验收 worktree 本身和外层记录、密钥不随普通任务收尾删除；它的 target 等构建缓存可以删除（2026-10-19 用户定，#240），下次 `run` 时在固定路径冷编译，不影响已有记录的校验。

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

被忽略的 `packaging/desktop-resources` 内容纳入指纹。桌面构建输出仅窄识别 `desktop/build.rs` 复制的 `desktop/dist/splash.js`、`desktop/dist/i18n.js` 及 `desktop/gen/schemas` 下的 `acl-manifests.json`、`capabilities.json`、`desktop-schema.json` 和 `macOS-schema.json`。这两份复制必须与已跟踪的普通文件 `web/static/splash.js`、`web/static/i18n.js` 逐字节一致；Tauri 文件必须为普通 JSON object 文件，首次由完整检查生成后记录各自内容和模式，签名记录同时绑定已跟踪的 build.rs、capabilities/permissions、Cargo.lock 等源输入及工具链。复用时这些已测输出也须逐项匹配，删除/篡改使复用失效；完整命令之后缺少要求输出不得记录成功。输入指纹不因这些具体输出从无到有而变动，产物证据与输入分别校验。旧候选的复制不一致时拒绝复用，但可由新的完整检查重新复制源并校验后登记；不泛化豁免整个 dist 或 gen，未知文件/符号链接仍拒绝。ONNX Runtime 1.30 导入会生成根目录 `:memory:.ses`，只将这一普通文件的时间戳/UUID 固定格式纳入同样的签名生成输出证据，不豁免任意 `:memory:*`；未知名称或格式拒绝，文件变化使复用失效。记录格式为 v3，旧 v1/v2 记录失效并需完整执行。未知忽略输入（例如外部 models/reference）直接拒绝，不读取内容；各已登记 crate（包括 crates/platform）的 target、工具 `.venv` 缓存、Python 字节码和 `.DS_Store` 属于可再生输出。固定区不得额外放置外部输入；若引入新的被忽略构建输入，应先修改策略和合成测试。旧图库升级输入使用 `tests/fixtures/library_upgrade` 中提交的历史 v5/v6 合成包；每项测试复制后升级，不在全量检查期间构建历史版本。

本机私有权限密钥对记录作 HMAC 校验，完整日志也校验摘要；缺字段、失败、定向命令、签名/日志篡改、输入或环境不匹配均拒绝。该记录用于防止误改/手工修改记录后的复用；拥有本机密钥和工具源码写权限的人不是安全隔离边界。不能把记录移到另一机器或平台冒称覆盖。密钥丢失则旧记录无效，从头完整运行。

PR 应明确区分“当前 head 上完整执行退出 0”和“当前候选仅验收叙述变化，复用已测 commit 的全量事实”，列出双方 commit/tree、固定路径、机器/平台/工具链、结果和跳过理由；其他平台与尚未做的 UI 验收单列。缓存收益只引用同一口径的实测时间；没有实测就不写提速数字。

## 合并队列（集成会话）

2026-10-18 起，合入 main 由一个集成会话统一做（用户决定见 [AGENTS.md](../AGENTS.md)）。起因：各开发会话分别在自己的 PR head 上跑全量，先合进一个，main 就变了，后面改到同一批文件的 PR 都要 rebase 再跑；并行的全量还互相抢机器。

**开发会话**：做完定向检查和平台相关验收（Windows 定向构建与测试、虚拟机走查、浏览器走查），在 PR 描述里写明结果和检查级别（界面 / 普通代码 / 数据层 / 只改文档），转为 Ready 并写「待集成」。不跑本机全量，不合并。前后叠着的 PR（例如同一件事拆成几块）在描述里写明顺序。

**集成会话**，每一批：

1. 从 `origin/main` 开候选分支 `integrate/<日期>-<序号>`，按顺序把这批 PR 的 head 逐个 `git merge --no-ff` 进来；合不上的退回原会话，写明冲突文件，这一批不带它。同一批里有前后叠着的 PR，按叠放顺序合。
   例外是迁移清单的指纹冲突（2026-10-18 用户批准）。`tests/migration_inventory.json` 每个 `sources` 指纹单独一行、指纹之间空一行（规范格式由 `python -m tools.migration_inventory fmt` 写出，`check` 和单元测试拦住不规范的格式），两个 PR 改不同文件的指纹时 git 会自动合并，只有两边改了同一个文件的指纹时才冲突。如果冲突只在这个文件，并且正在合入的 PR 相对共同基点只改了 `sources`，集成会话自己解决：取候选里的版本，再跑 `python -m tools.migration_inventory sync-sources`，它只把 `sources` 改成当前源码的指纹并按规范格式写回，不动边界和归属；然后确认 `python -m tools.migration_inventory report` 的 `warnings` 和 `errors` 都为空，提交合并。正在合入的 PR 如果还改了这个文件里的边界、归属或别的键，照旧退回原会话。GitHub 是逐个 PR 合并的，前一个 PR 合入后，这个 PR 在 GitHub 上仍会冲突，所以重算的结果要放到它的 head 上：候选里合入它的那个合并提交是它原 head 的后代，把这个提交快进推到它的分支，不用强推，测过的提交就是 PR head。推的是别的会话的分支，每次推之前先请用户同意，推完通知原会话合入前别再推送，并在 PR 里留评论说明。
2. 候选里只要有一个 PR 需要全量（普通代码或数据层），就在候选 head 上经 `collab_run.py` 跑一次 `tools/check_all.sh`；含数据层改动的，升级测试照常包含。整批都是界面级或只改文档时，按各自门槛检查（`node --test`、相对链接），不跑全量。
3. 全量失败时，先排除负载等环境原因（#219），再按 PR 二分定位，把失败的 PR 退回原会话，其余重新组批。
4. 通过后请用户批准合并。批准后按候选里的顺序，用 `gh pr merge --merge --match-head-commit <PR head>` 逐个合入；全部合完后核对 `origin/main^{tree}` 与候选 head 的 tree 相同，不同就停下报告。集成会话之外没有人合并，所以合并期间 main 不会被别人改动。
5. 在每个合入的 PR 里留一条评论：候选 head、同批 PR、全量结果（机器、系统、架构、工具链、通过与跳过）。删除已合并的远端分支和候选分支。
6. 清理本机 worktree（2026-10-19 用户定）：每个 worktree 带一份 Rust 构建产物，约 7～13 GB，不清理会把磁盘占满。这一批合入后，对 `git worktree list` 里分支已合入 `origin/main`、没有开着的 PR 以它为头或目标、`git status --porcelain` 为空（没有未提交和未跟踪的文件）的 worktree，执行 `git worktree remove` 并删除对应的本地分支；任何一条不满足就跳过，并在报告里列出。不清理固定验收区 `.venv/local-acceptance/worktree` 和集成会话自己的工作区；虚拟机母版和副本不在此列。报告里写明删了几个、腾出多少空间（`df -h /` 前后对比）。

平台专用改动（如 `cfg(windows)`）的定向验收由开发会话在自己的 head 上完成；组批时如果冲突解决碰到了平台专用代码，退回原会话补测，不由集成会话代做。

## macOS 实机测试（虚拟机）

2026-10-15 起，在 Mac 上启动 App 的实机测试都在 [Tart](https://tart.run) 虚拟机里做（用户决定见 [AGENTS.md](../AGENTS.md)）。虚拟机有自己的用户目录、应用数据和钥匙串，碰不到本机正式版的数据；本机只被占用磁盘，以及运行时的 CPU 和内存。

**母版与副本**：本机保留一台母版 `illustash-master`，来自 `ghcr.io/cirruslabs/macos-tahoe-base`（macOS 26），约 30 GB。镜像默认关闭 SIP，母版已在恢复模式里用 `csrutil enable` 打开 SIP；Gatekeeper 保持开启，母版里从没装过 Illustash。每次测试用 `tart clone illustash-master <名字>` 复制一份（APFS 写时复制，几乎不占额外空间），测完 `tart stop` 后 `tart delete` 删掉副本。母版只在用户明确同意时才修改或删除。镜像下载约 27 GB，大文件不经本机代理下载。

**启动与操作**：
- 启动用 `tart run --no-graphics --vnc-experimental [--dir=名字:路径:ro] <副本>`。不加 `--no-graphics` 时，tart 会在本机打开「屏幕共享」并占住 VNC 连接。VNC 只在测试期间开着，监听所有网卡，用的是随机口令。
- 同一时间只开一台虚拟机（2026-10-18，见 [AGENTS.md](../AGENTS.md)）：把上面的命令包在 `collab_run.py run --slot vm -- tart run …` 里放后台运行，虚拟机运行期间占着 vm 槽，`tart stop` 后释放；别的会话要开虚拟机时会排队，`collab_run.py status` 能看到是谁开着。不能前台运行的虚拟机（例如 UTM 的 `utmctl start` 会立即返回），用 `collab_run.py run --slot vm --` 包住一个等它关机的命令。不测的时候就关掉，不要让虚拟机空跑。
- 截图、点击、输入走 VNC，不需要虚拟机里的系统授权。命令走 `tart exec`：它用的是虚拟化通道，不依赖网络，以当前登录的用户身份运行。`tart exec` 第一次访问「下载」或管理用户时，虚拟机里会弹出授权框，要用 VNC 点允许。
- VNC 下滚轮和 Cmd 组合键不可靠：滚动靠拖滚动条，退出等操作走菜单栏；在原生文件选择器里直接输入 `/`，就能打开「前往文件夹」。

**验收用法**：
- 新用户首次安装：用 `sysadminctl -addUser` 建一个标准用户，再用 `sysadminctl -autologin set` 设为自动登录；重启后走完系统设置向导，之后的步骤都在这个账户里做。
- Gatekeeper：要让 App 像从网上下载的一样，就在虚拟机里用 `python3 -m http.server --bind 127.0.0.1` 提供 zip，再用 Safari 下载。这样文件会带上 Safari 的隔离标记；直接拷进虚拟机的文件没有这个标记。
- 断网：在虚拟机里用 `networksetup -setnetworkserviceenabled Ethernet off` 关掉唯一的网络服务（需要管理员权限）。这时 `scutil --nwi` 显示 Not Reachable，DNS 解析失败，等同拔掉网线；恢复时把 `off` 改成 `on`。
- 验收结果按平台写在 Issue 或 PR 里，注明这是虚拟机，以及构建的提交和安装包的哈希。

**做不了的**：iCloud 照片（虚拟机没登录 Apple 账户）；性能和识别速度（虚拟机里的数字没有参考价值）；Windows（Apple 芯片上只能跑 ARM 版 Windows，和用户的 x64 环境不同，Windows 仍在 x64 验收机上做）。

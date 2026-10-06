# 开发说明

状态：现行参考，2026-10-06。从 README 移出的开发者内容：构建、运行形态、图库包结构、目录、文档地图与检查流程。面向使用者的介绍见 [README](../README.md)。

核心是 Rust：`crates/core` 管业务规则、证据、人工裁决、关系、历史与投影，`crates/platform` 管平台能力，`crates/iqdb` 在本地算 IQDB 签名；`native/terms` 是应用宿主（CLI、HTTP、任务编排），`desktop/` 是只做启动与登录窗口的 Tauri 外壳。数据在 SQLite，推理固定 CPU ONNX Runtime，模型不进仓库、打包时按白名单加入。

文档、代码注释和提交信息里的 `#N` 编号、`PR #N` 与短提交哈希，凡早于 2026-10-06 开源的，都指向开源前的私有开发历史，在本仓库中查不到；本仓库的 Issue 和 PR 从 #1 重新编号。

## 三种形态

| 形态 | 入口 | 要点 |
|---|---|---|
| 本地 Web | `./start-web.command --library /路径/图库.pilib` | macOS 可双击；省略 `--library` 打开最近登记的图库；默认 `http://127.0.0.1:8765`，终端 Ctrl+C 停止 |
| 桌面 App | `desktop/`（Tauri 外壳） | 没有能打开的图库时显示欢迎页，可在界面里新建；承载 Pixiv 登录窗口 |
| 安装包 | `python3 packaging/build.py --output OUT` | 基础版只含核心、静态界面与标签表；`--research` 追加四类原生计算适配器 |

### 快速开始

```sh
# 构建核心
cargo build --release --manifest-path native/terms/Cargo.toml

# 首次建库：只登记来源，不扫描图片
./native/terms/target/release/illustash-terms library-create \
    --dest ~/Pictures/Illustash/我的图库.pilib --folder /图片目录

# 打开
./start-web.command --library ~/Pictures/Illustash/我的图库.pilib
```

`start-web.command` 在 `native/terms/target/{release,debug}` 里找 `illustash-terms`，也可以用 `ILLUSTASH_TERMS_BIN` 指定，找不到直接报错。服务和桌面入口都不隐式建库或升级；开库时按已保存的设置自动同步，`--no-auto-sync` 可关闭自动同步。多来源、Apple Photos 来源与出处分组见 [图库](LIBRARIES.md)。

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
| `--sync-limit N` | 每轮同步中待识别图片数上限，0 不限；不限制来源扫描 |
| `--no-names` / `--names-network` | 关闭名字同步 / 本次进程允许名称文字查询联网（图库也可在界面上持久授权） |
| `--no-provenance` / `--allow-provenance-network` | 不跑溯源任务 / 允许溯源联网 |
| `--no-index-check` / `--index-check` | 关闭 / 打开开库时的角色索引检查（默认自动） |
| `--index-network` | 索引检查允许联网 |
| `--native`、`--no-open` | 兼容参数，原生入口不做额外动作 |

已退役、直接报错的参数：`--python`、`--upstream`、`--route`、`--refresh`、`--source`、`--db`、`--cache`、`--identity`、`--no-similar` 和裸 `--`。恢复旧版本要停服务、保留当前包与 WAL，再从相容版本的升级前备份恢复；不要复制正在使用的 SQLite 文件。`tools/build_db.py` 重建的是分析源库，不是应用的更新入口。

### 安装包与桌面

`packaging/build.py` 按白名单复制资源，生成逐文件 SHA-256 的 `manifest.json` 与依赖清单；`--archive` 出 ZIP，`--binaries` 复用已构建的二进制，`--profile dev` 用调试构建。研究便携形态加 `--research --models PATH`；macOS 与 Windows 另需 `--ort-dylib FILE --ort-licenses DIR`，Windows 还需 `--openblas-site DIR`；桌面形态加 `--desktop PATH`，或加 `--desktop-resources` 后再在 `desktop/` 执行 `cargo tauri build --config ../packaging/tauri-bundle.json`。

2026-10-05 起 macOS 与 Windows 的四类计算适配器（识别、索引、学习/相似、溯源）全部原生执行，包内不带 Python 与计算源码；Linux 包交付计算源码，需显式配置外部 Python 3.12+。完整安装包、平台门槛与依赖边界见 [安装与平台](BACKEND_TASKS.md#独立安装与可选计算包)。

## 图库包

- `library.json`：包清单、来源与已移除来源记录。
- `source.sqlite`：来源快照，只读，不接收人工写入。
- `library.sqlite`：人工结论、机器证据与统一撤销历史，**不可当缓存删除**。
- `thumbnails/`：可重建；`backups/`：`--backup` 与升级前的备份。

包外的共享资源（参考库、模型、研究输出）与目录职责见 [图库参考](LIBRARIES.md#2-包外的共享资源)。

## 目录

```text
illustash/
├── crates/               词条核心、平台能力、IQDB 签名
├── native/terms/         应用宿主：Core 适配、CLI、HTTP、任务编排
├── desktop/              Tauri 桌面外壳：启动服务与 Pixiv 登录窗口
├── web/                  静态界面（web/static）与可选计算代码
├── provenance/           溯源计算与来源适配
├── packaging/            安装包与便携产物构建、资源白名单
├── tools/                构建、验收与研究入口（用途见 docs/TOOLS.md）
├── tests/                合成测试、契约夹具与历史参考实现
├── docs/                 现行参考、进行中的方案与待办
├── tags/                 标签表（tags_v3.csv）
├── start-web.command     本地 Web 入口（macOS）
├── AGENTS.md             协作流程与文档规范，CLAUDE.md 引入本篇
├── ERRATA.md             模型与调研材料的勘误
├── LICENSE               GPL-3.0 全文
└── README.md             对外介绍
```

本机忽略、不进仓库：`models/`（ONNX 模型）、`review/`（共享索引与研究数据）、`reference/`（研究参考材料）、`_scratch/`（可重新生成的产物）、`.venv/`（本机依赖与固定验收缓存）；`.claude/worktrees/` 放开发 worktree。

## 文档地图

现行参考都在 `docs/`；根目录只留 `README.md`、`AGENTS.md`、`CLAUDE.md`、`ERRATA.md` 和 `LICENSE`。

| 主题 | 文档 |
|---|---|
| 产品底线与待办 | [BACKLOG.md](BACKLOG.md) |
| 架构与迁移方向 | [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md) |
| 词条核心 | [TERMS.md](TERMS.md)、[TERMS_SCHEMA.md](TERMS_SCHEMA.md)、[TERMS_ARTISTS.md](TERMS_ARTISTS.md)、[TERMS_LABELS.md](TERMS_LABELS.md)、[TERMS_CHARACTERS.md](TERMS_CHARACTERS.md)、[TERMS_NAMES.md](TERMS_NAMES.md) |
| 图库与界面 | [LIBRARIES.md](LIBRARIES.md)、[WEB_V2.md](WEB_V2.md)、[WEB_NAMES.md](WEB_NAMES.md)、[DATA_MODEL.md](DATA_MODEL.md) |
| 识别与证据 | [RESULT_PACKAGES.md](RESULT_PACKAGES.md)、[EVIDENCE_PACKAGES.md](EVIDENCE_PACKAGES.md) |
| 溯源 | [PROVENANCE.md](PROVENANCE.md) |
| 安装与平台 | [BACKEND_TASKS.md](BACKEND_TASKS.md) |
| 验收与性能 | [LOCAL_CHECKS.md](LOCAL_CHECKS.md)、[TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md) |
| 角色与名字迁移参考 | [TERMS_CHARACTERS_PLAN.md](TERMS_CHARACTERS_PLAN.md)、[TERMS_NAMES_PLAN.md](TERMS_NAMES_PLAN.md) |
| 工具与许可 | [TOOLS.md](TOOLS.md)、[THIRD_PARTY.md](THIRD_PARTY.md) |
| 模型勘误 | [ERRATA.md](../ERRATA.md) |

分类与维护规则见 [AGENTS.md](../AGENTS.md) 的「文档维护规范」：仓库只留现行参考、进行中的方案、待办与约束三种文档，任务与流水放 GitHub。

## 开发与验收

- 工具链：Python 3.12、Node 22、Rust stable（含 rustfmt/clippy）；Python 依赖与历史构建缓存放在被忽略的 `.venv/check-all/`。
- 开发期和合入其他分支的 PR 按改动跑定向检查；目标为 main 的非纯 Markdown PR 先完成其他平台检查，再在已提交的干净候选上跑一次本机完整入口 `bash tools/check_all.sh`（对齐 `.github/workflows/ci.yml`）。固定验收区用 `python3.12 tools/local_acceptance.py run <候选SHA>` 串行执行并写签名记录，`result <候选SHA>` 取结果。
- 允许的跳过只有 CI 同样允许的平台限制和真实图库数据缺失；不访问真实图库补齐数据。
- 只改 Markdown 的 PR 不跑全量，只检查相对链接。
- GitHub Actions 只有 `workflow_dispatch`，且 job 要求运行 ref 为 `refs/heads/main`；推送、开 PR、合并都不自动运行。
- 常用入口：核心测试 `cargo test --locked --manifest-path native/terms/Cargo.toml`，Python 合成套件 `python -m tests.run_offline -v -b`（用装了依赖的 3.12 解释器）。

细节与复用口径见 [本地全量验收](LOCAL_CHECKS.md)。

## 协作

任务单、进度、审核与验收都放 GitHub（`shio-vinyl/illustash`）：任务在 Issue，进度与验收在草稿 PR 描述，交接在最新一条评论，需要用户决定的事在 Issue 里提问并打「待决定」标签。仓库里只放现在为真的东西，规则见 [AGENTS.md](../AGENTS.md)。


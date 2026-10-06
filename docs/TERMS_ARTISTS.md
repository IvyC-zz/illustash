# 画师收编（词条第 2 步）

状态：现行参考，2026-10-01。[TERMS.md](TERMS.md) 第 4 节第 2 步的画师收编、投影、裁决与证据包契约。

当前生产入口由 Rust 负责，普通启动不做 Python 后台切换；升级显式使用 `http --upgrade`。本文提到的 Store/adopt_* 是保留的测试参考，原生现行行为见 [架构方向](ARCHITECTURE_DIRECTION.md#生产入口与测试参考)。

核心持有画师词条和人工裁决，投影与历史保持既有契约；溯源 worker 只交机器证据包。旧表冻结副本自词条 schema 4 起不再保留，恢复使用升级前一致性备份（见第 3 节）。

## 1. 收编映射

`crates/core/src/legacy_artists.rs`，命令行：

```sh
illustash-terms migrate --db <library.sqlite>
illustash-terms sync-artists --db <library.sqlite> --provenance <provenance.sqlite>
illustash-terms reproject --db <library.sqlite>     # 需要时整表重建投影
```

只读溯源库（以 `mode=ro` URI 挂上），library.sqlite 的 `assets`、`actions`、`artist_verdicts` 只查不改；一个事务，失败整次回滚。可以反复运行，每次把核心补到与旧表一致：

| 旧 | 核心 |
|---|---|
| `artists` | `terms`（kind=artist，key 沿用画师 UUID，`#/?artist=<id>` 不变），加一条 `provenance:<id>` 链接 |
| `artist_links` | `term_links`，跟随旧表（移到另一位画师就跟着移；挂在别的类型词条上则拒绝） |
| `artist_names` | `term_names` 显示名，每个 (画师, 来源) 一个，跟随旧表 |
| `merged_into` | `same_as` 关系证据（方法 `merge`） |
| `image_artists` | 证据：来源 `provenance`，方法照旧；量纲 `exact_match`（像素哈希）、`gallery_similarity`（本地比对）、`ocr_confidence`（OCR）；`status` 记在证据 JSON。旧表没有算法版本，版本用内容摘要 `v4/<摘要>`：行被改写后另起一条，旧的留着 |
| `artist_verdicts` | 裁决：按 `actions` 历史重建，每条挂在原来那一步上，来源 `legacy`；已撤销的步骤不收 |
| `assets` | `term_assets`，命名空间 `library`（键是 `assets.uuid`，不分 Photos 与文件夹）；不在 `assets` 里的记为已移除，证据和裁决照留、不投影 |

**裁决重建**：某一对 (图, 画师) 在未撤销历史里的每一步，设成的值等于下一步记下的 `had`；最后一步设成的就是 `artist_verdicts` 当前的值。每次同步后做**全体核对**：核心里所有画师的有效裁决必须与 `artist_verdicts` 完全相等，否则整次回滚。它能发现绕过历史直接改表的情况；找不到历史步骤的判断也拒绝。Python 撤销旧步骤只改 `actions.undone`，所以每次同步都重投所有带旧裁决的图。

为此把「有效裁决」改为按 `action_id`（历史顺序）取最新，同一步内再按插入顺序；补收的旧历史插入顺序不等于历史顺序。schema v1 尚未发布，直接改了 v1 的视图。

## 2. 逐表对照

`tools/verify_terms_artists.py --library <副本>`（2026-10-05 随参考实现删除，#186；见 git 历史）：调用 `tests/reference/web/artists.py` 的原函数（不另写一份旧逻辑）与核心投影比较；拒绝在 `~/Pictures/Illustash` 下运行。

**2026-09-28，「我的图库」一致性副本**（SQLite 备份接口只读拷贝 library.sqlite 与 provenance.sqlite；拷贝前后核对原库未被写）：

| 项 | 比较数 | 差异 |
|---|---|---|
| 每张图显示的画师与「已确认」 | 8,316 张 | 0 |
| 「库里 N 张」 | 3,269 位 | 0 |
| 按画师筛选出的图 | 4,011 位 | 0 |
| 抽查池 | 8,316 张 | 0 |
| 链接（system, key, url） | 4,011 位 | 0 |
| 昵称 | 4,011 位 | 0 |

规模：约 4.8 万张图、4,011 位画师、9,420 条归属（像素哈希 8,316、OCR 1,104，本地比对 0）、49 条「是」、0 条「不是」、50 对有历史。耗时：首次同步 0.7 秒，重跑 0.3 秒，整表重投 0.9 秒；旧读路径把这些算一遍 23.7 秒，核心 0.15 秒。

真实数据里没有「不是」，所以另在副本上补做：
- 用旧写路径 `Store.judge_artist` 判 3 张「不是」（其中 1 张原来是「是」）、收回 1 张，同步后 6 项仍全部一致；
- 用 Python `Store.undo` 撤销那步收回，同步后一致；
- 故意删掉一条投影，脚本在显示、筛选、计数三项报出差异；`reproject` 后恢复一致。

没有对照的：原帖（`image_sources`）仍只在溯源库，核心不收；显示名的选择顺序（昵称 → X 账号 → Danbooru tag）仍在 `tests/reference/web/artists.py`，这里只比原料。Windows 未验收。

## 3. 收编写路径

**切换**（`legacy_artists::cutover`，命令行 `cutover-artists`）：建表或升级、最后一次收编同步，并在**同一个事务**里写下 library.sqlite 的 `meta` `terms_artists=core`。已经切过就什么都不做；切过之后 `sync-artists` 拒绝运行（核心新写的裁决不在冻结的旧表里）。没有 provenance.sqlite 的图库也能切（按空的溯源库处理，只收人工判断）。

历史 Python 测试参考的服务启动时（`tests/reference/web/server.py` `adopt_artists`）：图库还没切、且找得到核心程序，就先 `Store.backup()` 备份人工知识，再切换，打印收了多少画师、证据和判断；找不到程序或切换失败，照旧用旧表并打印原因。桌面 App 里核心程序就是外壳本身（`ILLUSTASH_TERMS_BIN`）。

**切换之后：**

| | 之前 | 之后 |
|---|---|---|
| 「是 TA / 不是 / 清除」 | Python 写 `artist_verdicts` | `Store.judge_artist` 校验后调用核心 `judge`，写 `term_verdicts` 和一步 `actions`；历史文案不变 |
| 撤销 | Python 按 `before_json` 改回 `artist_verdicts` | 核心写的步骤、以及旧的 `artist_verdict` 步骤（收编时已挂在同一个 action 上）都交给核心撤；其他步骤仍归 Python |
| `Store.artist_verdicts()` | 读 `artist_verdicts` | 读核心的有效裁决，形状不变 |
| 溯源结果 | 核心直接读 `image_artists` | 每次运行结束（含停止、失败），以及服务启动时，Runner 调 `tests/reference/provenance/export.py` 写一个完整快照证据包，交核心 `import`；核心按指纹去重。失败只记录（`/api/provenance` 的 `delivery`）不抛出，下次再交 |
| 详情里的画师、库里 N 张、按画师筛选、抽查池 | `tests/reference/web/artists.py` 读 `image_artists` + 判断 | 读核心投影（机器认定、人工确认可见；抽查池是有强证据的图） |
| 画师名字与主页 | 读 `artist_links`、`artist_names` | 读 `term_links`、`term_names`，选名规则不变（`_card` 两边共用） |
| 原帖 | `image_sources` | 不变，仍读溯源库；「不是」的判断改用核心投影 |

与旧行为的差别：人对只有 OCR 线索的图说「是」，现在会显示为已确认并计入「库里 N 张」（旧逻辑只看 `auto`，这样的图不显示）；「不是」的图在抽查页照旧带标记显示。

**证据包的交接**：包里用 `provenance:<画师 id>` 链接引用画师，`terms.jsonl` 带上各画师的外部链接和昵称；新画师按这条链接新建时 key 就是画师 id，界面链接不变。导出器的映射必须与收编同步逐字节一致（来源、方法、量纲、数据、内容摘要版本），否则切换后第一个包会把每条归属再加一遍。第一次在真实副本上试时就有 22 条对不上：Python `json.dumps` 与 serde_json 对同一个浮点数的排版不同（`2.1e-09` 对 `2.1e-9`、`5.7e-05` 对 `0.000057`）。导出器的规范化 JSON 现在按 serde_json（ryu）的规则排版浮点数，两边各有一个测试钉在同一组字面值上。

**兼容结构**：词条 schema 4 起不再保留切换前冻结副本，回退使用一致性备份（见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 第 4 节）。`image_artists` 仍是溯源 worker 的机器缓存，人工裁决由核心持有；其他兼容表/视图按实际消费者另行清理，不随 HTTP 宿主退役删除。

**回退**：先停止服务并保留当前包与 WAL，再使用切换/升级前的 `backups/` 一致性备份恢复相容版本；不能通过删除 `terms_artists` 标记退回旧宿主。

### 验收（2026-09-28，「我的图库」一致性副本）

- 切换：0.7 秒，4,011 位画师、9,420 条证据、51 条裁决（50 对历史），全体核对通过。
- 切换后第一个证据包：9,420 条全部识别为已有（`added 0, duplicate 9420`），约 2.3 秒；同一快照再交一次直接判为已导入。
- 三方对照（`verify_terms_artists.py --baseline` 未切换的副本）：旧读路径 vs 核心表 vs App 实际读路径（切换后的 `tests/reference/web/artists.py`），显示与确认 8,316 张、库里 N 张 3,269 位、筛选 4,011 位、抽查池 8,316 张、链接昵称与显示名 4,011 位，**全部 0 差异**。旧读路径全量 24.2 秒，切换后的 App 读路径 18.2 秒。
- 测试：Rust 48 项；Python 全量 401 项（新增 `tests/test_terms_artists.py`：切换、核心读写、撤销新旧步骤、交包、没有核心、交包失败、核心拒绝的提示）。

没有做的：没在真实图库上切换（它会在你下次用这个分支启动 App 时自动进行，先备份）；没启动完整桌面 App；Windows 未验收；同一位画师的合并（`same_as`）还不会在投影里归并，当前真实数据里没有合并过的画师。

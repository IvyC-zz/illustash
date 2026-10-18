# 角色、皮肤 / 变体、作品收编（词条第 4、5 步）

状态：现行参考，2026-10-01。[TERMS.md](TERMS.md) 第 4 节第 4、5 步的角色、皮肤与作品收编契约，按用户决定合并；相关契约见 [TERMS_ARTISTS.md](TERMS_ARTISTS.md) 和 [TERMS_LABELS.md](TERMS_LABELS.md)。

当前生产入口由 Rust 负责，普通启动不做 Python 后台切换；升级显式使用 `http --upgrade`。本文提到的 Store/adopt_* 是保留的测试参考，原生现行行为见 [架构方向](ARCHITECTURE_DIRECTION.md#生产入口与测试参考)。

本文是结果说明。方案和决定（D1–D6）在 [TERMS_CHARACTERS_PLAN.md](TERMS_CHARACTERS_PLAN.md)，性能现状在 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)，schema 在 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 7、8 节。

**范围**：
- 角色（人物、原创角色、吉祥物、只当标签）、皮肤 / 变体、作品树；
- 人工结论（含相册导入）、身份关系与决议（hybrid）、候选、学习产物，以及由它们算出的归属。

**不在范围**：名字按 D4 这一步不收编，表和选名规则都还在 Python，核心命令只按原语义写用户名字和别名（方案第 10 节是下一步的契约）。身份策略 v2 按 D1 不支持。

## 1. 映射

原则是**等价迁移**：旧表的每个读者读到的东西不变，规则原样执行。旧表变成由核心维护的**兼容表**（同名同形、加写保护），核心另存主数据。

| 旧 | 核心主数据 | 切换后 Python 读到的 |
|---|---|---|
| `entities` | `terms`（key = 原 id，kind 原样，variant / artist / label_only / mascot / mascot_origin / original 进 `attrs`）+ `term_tree`（parent_id、merged_into、work_id、work 文字，以及 work_review 三列） | 兼容表 `entities`，列序保留旧库自己的（真实库由 `ALTER TABLE` 历史决定） |
| `links` | `term_links`（角色的 `wd` 存成 `wd_character`，其余原样） | 兼容表 `links`。计划里是视图；实测表的查询计划与旧版相同，就保留为表（D5） |
| `work_review` | `term_tree` 的三列 | 兼容表 |
| `identity_baseline` | `term_identity_baseline`（`seq` = 原 rowid 顺序） | 兼容表 `identity_baseline`：旧代码按原样读它，核心随主表追加（更正了方案第 4.1 节的「改名冻结」） |
| `assertions` | `term_verdicts`：confirmed → yes，rejected → no，原 origin 存进 `via`。有步骤的挂在原来那一步的 `action_id` 上；没有步骤的进批次（`baseline`、`album`、`migration`、`undo_audit`），按锚点排进时间线 | 兼容表 = 每个目标的有效裁决 |
| `identity_relations` / `identity_evidence` / `identity_decisions` / `identity_decision_evidence` | `term_relations`（`duplicate_of` → `same_as`，`form_of` → `contains` 端点反转，`different_from` 原样）、关系证据、关系裁决（`reviewed` = 当时看到的证据），旧 id 记在 `term_legacy_ids` | 兼容表，保留原 id；status 由最新裁决算出 |
| `candidates` | 证据：WD → `wd.wd_character`，CCIP → `ccip_index.char_ext`，按**证据批次**生效（每次 refresh / reindex / import 是一批，覆盖到的图上以这一批为准） | 兼容表：有效证据按 (图, 身份, 来源) 取最大分 |
| `proto_claims`、`weak_claims`、`prototypes`、`learn_state` | 判定器私有状态，由学习批次整批替换 | 原表，核心写 |
| `identity_canonical`、`identity_scopes`、`identity_suspended`、`identity_uncertain`、`identity_claims`、`identity_anchor_*` | 判定器私有状态 | 原表，核心写 |
| `memberships`、`tag_memberships`、`family_evidence`、`single_assets`、`lead_assets` | 投影：核心执行 `crates/core/src/character_sql/` 里逐字保存的旧 SQL；按图增量重算时把范围条件插在固定锚点上，锚点不是恰好出现一次就报错，不会悄悄退回整库（#200）。合成数据上增量与全量逐行一致由 `native/terms/tests/character_projection.rs` 钉住。人物、皮肤的归属另外镜像进 `term_projection`（目前没有读者） | 兼容表 |
| `membership_rules` 等规则视图 | — | 原样 |
| `names` 等名字表、`asked`、`reviews`、`deferred`、`duplicates`、`auto_rules`、`excluded`、`imported`、`backend_effects`、`prefs` | 外围，不收编 | 原表；属于某一步的由那一步的核心命令同事务写，`curate`、跳过配对、NameUpdater 仍由 Python 写 |
| `relations` | 图片相似关系，与本步无关 | 不变 |

加写保护的兼容表和私有表共 27 张（`legacy_characters::GUARDED`）：没有核心的写钥匙时 INSERT / UPDATE / DELETE 一律拒绝，漏改的 Python 写入会直接报错，不会悄悄成功。

**收编同步**（`illustash-terms sync-characters --db <库>`，切换前可以反复运行）：只读旧表，把 `term_*` 补到与旧表一致，再由 `term_*` 生成影子兼容表 `shadow_*`，与旧表逐行逐列比较。一个事务：预检不通过、有出处无法解释的人工结论、比较有差异，都整次回滚并报告（退出码 3）。旧表没变时重跑不新增任何行。规则细节（hybrid 下旧合并搬动的结论放进迁移批次、没有步骤的关系决议怎样排锚点等）见方案第 16 节 S2。

`verify-characters` 从 `term_*` 全量重建兼容表并比较，切换前后都能跑，用来核对核心自身。

## 2. 切换

`legacy_characters::cutover`（`illustash-terms cutover-characters`），一个事务：

1. 前提：标签已经切到核心（`general_hits`、`asset_rating` 是视图）；身份策略未设置或 hybrid（v2 拒绝，留在旧表）。
2. 最后一次收编同步，必须零差异。
3. （第 4 版之前）旧人工表冻结一份：`legacy_assertions`、`legacy_identity_relations` 等 5 张，外加 `legacy_identity_baseline`。**词条 schema 第 4 版（2026-09-29，拆脚手架阶段 A）起不再冻结**，已有的冻结副本在升级时删掉（第 4 节）。
4. 删掉 `assertions` 上的身份触发器（它们的语义由核心在写结论的同一段代码里执行），删掉 `shadow_*`。
5. 27 张表加写保护。
6. 按当前兼容表整片建人物 / 皮肤的 `term_projection` 镜像（S5 发现并补上，见第 6 节）。
7. 写 `meta` 的 `terms_characters=core`。

**历史 Python 测试参考的服务启动时**（`tests/reference/web/server.py（#9 已删除）`）：`initialize` → 画师切换（`adopt_artists`）→ 补交溯源证据包 → 页面可用 → 后台 `adopt_terms`：先标签、再角色。
- 每次切换前先备份；
- 角色切换全程持有进程写锁（`Store._labels_lock`），期间写操作等待，读照常；
- 失败或中途退出都留在旧表，下次启动再试。在真实副本上用桌面 App 验证过：标签交 WD 证据中途 App 被关掉，库完好、仍是旧表，重开后接着交完并切换。

## 3. 切换之后

**写入口**：`tests/reference/web/terms_route.py（#9 已删除）` 的 `via_core` 包住 Store 的写方法：`change`、`answer`、`answer_audit`、`reassign`、`sweep`、`correct_identity_anchor`、`decide_identity`、`uncertain_identity`、`merge`、`create_entity`、`create_linked`、`set_profile`、`link_local`、`shared_oc`、`set_kind`、`set_mascot`、`set_work`，另外 `edit_work` 在方法内分支。

做法是先在一个「只演不写」的连接上跑一遍旧代码（总是回滚）。拿到的东西有：
- 旧的校验和拒绝文案；
- 历史文案；
- 新身份的 id；
- 联网查到的版权。

然后组装一个 JSON 命令交给核心（`illustash-terms character-command`），核心在一个事务里执行：
- 写兼容表；
- 追加 `term_verdicts` / 关系裁决；
- 镜像 `terms` / `term_tree` / `term_links`；
- 执行旧的投影 SQL；
- 写一步 `actions` 和 `backend_effects`。

命令一览见方案第 16 节 S3。性能优化后，`change` 等 8 个命令的预演跳过末尾的派生表刷新：刷新之后只读实体名和复制品组来写文案，真正的刷新在核心里做（[TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)）。

**历史与撤销**：
- 核心写的步骤是一项 `{"terms": {"v": 2, "type": …, "before": [旧形状…]}}`，历史文案照旧由 `names.action_label` 从里面的旧快照渲染。
- `Store.undo` 按 `terms_core.character_owns`（与 Rust `character_commands::owns` 同一条规则）判断归属：新信封，以及每一项都是角色形状（快照、`merge`、`identity_relation`、`identity_anchor`、`profile`）的旧步骤，都交核心撤；角色形状和别的形状混在一步里，或形状不认识，拒绝，什么都不改。
- `[]`、`curation`、`identity_skip` 仍归 Python；画师、标签步骤归各自的核心撤销。
- 旧历史可以按原样一步步撤到最早。

**导入与重扫**：`refresh`、`reindex_characters`、`import_changes`、`refresh_general_hits`（派生重算）、`apply_work_evidence`（原 `tools/reconcile_works.py`，2026-10-05 删除，#186）都改走核心。Python 在写锁内用只读连接准备输入（查询和顺序照旧），候选写成证据批次。

**学习**：Learner 经核心发布学习批次（`learning-snapshot` / `publish-learning`）。发布前按指纹检查快照是否过期，过期就不写、稍后重来（D6，唯一纳入的行为变化，界面看不到）。

**拒绝的操作**：
- 切换后拒绝激活或改身份策略、拒绝旧的 schema 升级（`_migrate`）；
- 旧 Python 索引工具已在 #53 删除。当时在副作用之前拒绝的离线工具（`apply_classification_repair`、`preview_classification_repair`、`identity_rollout`、`identity_bank`、`identity_metadata`、`eval_bench`）已于 2026-10-05 删除（#186）。

**切换之后仍由 Python 负责的**：`curate`（收藏排除）、跳过配对、名字的联网补全（NameUpdater）、复制品组和相似组的计算（核心只接收结果）。

## 4. 回退

- 切换前的自动备份（`library-backups/`）可以整库恢复。**切换之后做的操作会丢失**（D3）。
- 冻结的旧表不再保留（词条 schema 第 4 版，用户 2026-09-29 决定）：画师的 `artist_verdicts`、标签的 `legacy_asset_tags`、角色的 `legacy_*` 在切换时直接删掉；第 4 版之前切换的库，在下次启动升级词条表时删掉。切换前的数据只在自动备份里。
- 服务每次启动都先升级词条表（`tests/reference/web/server.py（#9 已删除）` `upgrade_terms`）：已切换的库不会再经过 `adopt_*` 的迁移，而核心命令拒绝旧版本的库。
- 切换是单向的：用这个分支打开过的图库，main 的旧代码读不到切换后的数据。

## 5. 与旧行为的差别

都已由用户确认或批准，每条都有单独的测试或对照钉住：

1. **学习发布时 ≤ 1e-6 的分数变化不刷新归属**（旧版规则，原样保留）。真实库里有 1 张图处在这种状态，全量重算与增量结果差这一行，镜像也照增量结果。
2. **hybrid 下撤销身份合并**：`identity_claims` 拆组新增的未定图片没有补进刷新范围（旧版规则，原样保留），增量里留着一行、全量重算没有。
3. **撤销画师 / 标签步骤时不排 `verdict` 效果**：旧版因为 `Store.undo` 整体挂着装饰器，会多排一条唤醒角色学习的效果；切换后这些步骤由核心撤销，不排（第 2 步起的设计，2026-09-29 用户确认保留）。
4. **表示方式的变化（非行为）**：新步骤的历史写成信封（上文），零成员 sweep 的快照也在信封里。

## 6. 验收

| 段 | 内容 | 结果 |
|---|---|---|
| S0 | 在旧代码上钉住现有行为的 30 个夹具（作品反例、跨层合并、相册与撤销、旧合并在 hybrid 下撤销、零成员 sweep、混合步骤、空的成功批次……） | 旧代码通过；切换后原样再跑（`Switched*`）通过 |
| S1–S3 | schema v2、影子收编、全部命令与统一撤销的双跑 | 每步兼容表、派生表、私有状态、历史逐表相等；每条命令后全量重建等于增量；连续撤到最早一步都相等；核心中途报错什么都不留 |
| S4 | 切换与接线 | 写保护、工具拒绝、重启、失败留旧表并能重试；17 个已有测试模块切换后再跑 |
| S5 | 「我的图库」一致性副本（约 4.8 万张图、9,276 个身份、1,459 步历史，hybrid） | 收编 0 差异、出处无法解释 0；真实历史连续撤销 50 步逐表相等；27 种操作加 8 次撤销双跑一致；旧读路径、核心、App 读路径三方 0 差异（约 1.7 万次接口调用，画师 19,412 张 / 6,078 位 / 抽查池）；每项都做了故意破坏，都能报出。性能优化后重跑，结果相同 |
| 桌面 App | 真实副本上用 worktree 的桌面外壳启动（关同步、补名字、找相似、索引检查，不给联网授权；「最近打开」列表隔离） | 外壳兼任的核心程序被找到；画师、标签、角色依次切换，中途被打断后重开能续上；图库、角色、角色筛图、查看大图、设置页正常，控制台无错误；界面上做一次「不是 TA」再撤销，都经核心，`verify-characters` 零差异 |

S5 在真实数据里修掉两个实现 bug：
- 收编预检按列的**顺序**比较，真实库列序不同就被拒绝；改成按列名比较。
- 切换时没有建人物 / 皮肤的投影镜像；改成切换事务里整片建一次。

对照工具（都拒绝在 `~/Pictures/Illustash` 下运行；2026-10-05 随参考实现删除，#186；见 git 历史）：
- `tools/verify_terms_characters_undo.py`：连续撤销对照；
- `tools/verify_terms_characters_ops.py`：抽样操作双跑与逐操作计时；
- `tools/verify_terms_characters.py`：切换后三方对照；
- `tools/bench_terms_characters.py`：合成副本基准。

测试（2026-10-07 起，#9）：
- `native/terms/tests/replay.rs`：原 Python 契约与双跑测试（`test_terms_characters_contract`、`test_terms_character_*`、`test_terms_characters_cutover`、`test_identity*`、`test_lead`、`test_web_v2`、`test_works` 等，按原模块名各一项）在仍通过固定快照时录下的全部核心调用，用当前 `illustash-terms` 重放，逐步对照输出和各表状态摘要。录制无法重做：参考实现已删除。夹具在 `native/terms/tests/fixtures/replay/`，比较口径写在 `replay.rs` 开头；
- Rust `crates/core/src/legacy_characters/tests.rs` 等。

## 7. 体积与性能

真实规模（2026-09-29 切换时的测量，早于两轮性能优化；现状见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)）：

- **首次启动的切换**约 2 分钟：画师 2 秒、交包 3 秒、标签 107–134 秒（在后台；桌面 App 里与其他后台任务一起跑时 378 秒）、角色 13–27 秒。
- **库**：451.5 MB → 1,098.6 MB，主要是标签那一步的 257 万条 WD 证据；切换期间 WAL 最大约 262 MB，另外每次切换前各有一份自动备份。
- **日常裁决、sweep、回答**与旧版相当。关系决议、合并约为旧版 2 倍；全量 refresh 约 3.4 倍（57 秒）；新建角色 2.6 倍；reindex / 导入约 2.3 倍；撤销合计 1.6 倍。
- `links` 的关键查询计划与旧版相同。

## 8. 没有做的

- 名字投影（下一步，契约见方案第 10 节）；身份策略 v2；Windows 验收。
- 删旧表：冻结表已在第 4 版删掉；兼容表还在，要等 Python 读路径改读核心投影之后才能去掉。
- 性能待优化清单见性能文档。
- 回合 main 由用户决定。回合后第一次打开真实图库会自动切换（先备份）。

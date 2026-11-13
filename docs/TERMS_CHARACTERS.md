# 角色、皮肤 / 变体、作品收编（词条第 4、5 步）

状态：现行参考，2026-10-20。[TERMS.md](TERMS.md) 第 4 节第 4、5 步的角色、皮肤与作品收编契约，按用户决定合并；相关契约见 [TERMS_ARTISTS.md](TERMS_ARTISTS.md) 和 [TERMS_LABELS.md](TERMS_LABELS.md)。

当前生产入口由 Rust 负责，升级显式使用 `http --upgrade`。第 2 节和第 5–7 节提到的 Python（`Store`、`adopt_*`）是切换当时的旧实现，已随 #9 删除；现行写入口见第 3 节。

本文是结果说明。方案里仍然有效的设计（移植的规则、证据批次、写入口与撤销适配）和用户决定（D1–D6）在 [TERMS_CHARACTERS_PLAN.md](TERMS_CHARACTERS_PLAN.md)，性能现状在 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)，schema 在 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 7、8 节。

**范围**：
- 角色（人物、原创角色、吉祥物、只当标签）、皮肤 / 变体、作品树；
- 人工结论（含相册导入）、身份关系与决议（hybrid）、候选、学习产物，以及由它们算出的归属。

**不在范围**：名字按 D4 这一步不收编，核心命令只按原语义写用户名字和别名；名字在之后的名字那一步收编（[TERMS_NAMES.md](TERMS_NAMES.md)）。身份策略 v2 按 D1 不支持。

## 1. 映射

原则是**等价迁移**：旧表的每个读者读到的东西不变，规则原样执行。旧表变成由核心维护的**兼容表**（同名同形、加写保护），核心另存主数据。核心第 8 版（#201）起，身份、链接、作品复核、身份基线和身份关系的兼容表已删除，命令直接读写核心；留下的 `assertions`、`candidates` 和归属表定为核心维护的派生表，判定器私有状态照旧（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13、14 节）。

| 旧 | 核心主数据 | 现在 |
|---|---|---|
| `entities` | `terms`（key = 原 id，kind 原样）+ `term_tree`（parent_id、merged_into、work_id、work 文字，variant / artist / label_only / mascot / mascot_origin / original，先后 `seq` = 原 rowid，以及 work_review 三列） | 核心第 8 版起删除，命令直接读写 `terms` / `term_tree`，读者用同形的片段 `character_entities::ENTITIES`（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节）；之前是兼容表，属性在 `terms.attrs` 里 |
| `links` | `term_links`（角色的 `wd` 存成 `wd_character`，其余原样） | 核心第 8 版起删除，核心直接读写 `term_links`（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节） |
| `work_review` | `term_tree` 的三列 | 核心第 8 版起删除，命令直接读写 `term_tree`（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节） |
| `identity_baseline` | `term_identity_baseline`（`seq` = 原 rowid 顺序） | 核心第 8 版起删除（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节）；之前是旧代码按原样读的兼容表，核心随主表追加 |
| `assertions` | `term_verdicts`：confirmed → yes，rejected → no，原 origin 存进 `via`。有步骤的挂在原来那一步的 `action_id` 上；没有步骤的进批次（`baseline`、`album`、`migration`、`undo_audit`），按锚点排进时间线 | 核心维护的派生表 = 每个目标的有效裁决（第 8 版起不算兼容层，[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节） |
| `identity_relations` / `identity_evidence` / `identity_decisions` / `identity_decision_evidence` | `term_relations`（`duplicate_of` → `same_as`，`form_of` → `contains` 端点反转，`different_from` 原样）、关系证据、关系裁决（`reviewed` = 当时看到的证据），旧 id 记在 `term_legacy_ids` | 核心第 8 版起删除，命令只写核心，读者用同形的片段 `character_relations`（保留原 id；status 由最新裁决算出，[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节）；之前是兼容表 |
| `candidates` | 证据：WD → `wd.wd_character`，CCIP → `ccip_index.char_ext`，按**证据批次**生效（每次 refresh / reindex / import 是一批，覆盖到的图上以这一批为准） | 核心维护的派生表：有效证据按 (图, 身份, 来源) 取最大分（第 8 版起不算兼容层，[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节） |
| `proto_claims`、`weak_claims`、`prototypes`、`learn_state` | 判定器私有状态，由学习批次整批替换 | 原表，核心写 |
| `identity_canonical`、`identity_scopes`、`identity_suspended`、`identity_uncertain`、`identity_claims`、`identity_anchor_*` | 判定器私有状态 | 原表，核心写 |
| `memberships`、`tag_memberships`、`family_evidence`、`single_assets`、`lead_assets` | 投影：核心执行 `crates/core/src/character_sql/` 里逐字保存的旧 SQL；按图增量重算时把范围条件插在固定锚点上，锚点不是恰好出现一次就报错，不会悄悄退回整库（#200）。合成数据上增量与全量逐行一致由 `native/terms/tests/it/character_projection.rs` 钉住。人物、皮肤的归属另外镜像进 `term_projection`（目前没有读者） | 核心维护的派生表（第 8 版起不算兼容层） |
| `membership_rules` 等规则视图 | — | 原样 |
| `asked`、`reviews`、`deferred`、`duplicates`、`auto_rules`、`excluded`、`imported`、`backend_effects`、`prefs` | 外围，不收编 | 原表；属于某一步的由那一步的核心命令同事务写，收藏排除、跳过配对不走角色命令 |
| `relations` | 图片相似关系，与本步无关 | 不变 |

加写保护的派生表、判定器私有表和核心私有表共 21 张（`legacy_characters::GUARDED` 18 张、`schema::PRIVATE` 3 张）：没有核心的写钥匙时 INSERT / UPDATE / DELETE 一律拒绝，漏改的写入会直接报错，不会悄悄成功。

**收编同步**（原来的 `illustash-terms sync-characters --db <库>`，切换前可以反复运行；与 `verify-characters`、`cutover-characters` 一起随 #201 第 4 步删除，见第 2 节）：只读旧表，把 `term_*` 补到与旧表一致，再由 `term_*` 生成影子兼容表 `shadow_*`，与旧表逐行逐列比较。一个事务：预检不通过、有出处无法解释的人工结论、比较有差异，都整次回滚并报告（退出码 3）。旧表没变时重跑不新增任何行。人工结论怎样分进批次、排锚点见方案第 6.2 节。

`verify-characters` 原来从 `term_*` 全量重建兼容表并比较。第 8 版升级时曾沿用同样的核对（`legacy_characters::verify_in`，TEMP 影子），随 #201 第 4 步删除：被删的旧表在退役时各自与核心核对，留下的派生表随时可以由权威重算。

## 2. 切换

**这条命令和收编代码已随 #201 第 4 步删除（2026-10-20）**：核心第 8 版拒绝没切换完的图库，新建图库由 `new_library` 一开始就在核心里（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节）。下面是当时的过程，现有图库都是这样切过来的。

原来的 `legacy_characters::cutover`（`illustash-terms cutover-characters`），一个事务：

1. 前提：标签已经切到核心（`general_hits`、`asset_rating` 是视图）；身份策略未设置或 hybrid（v2 拒绝，留在旧表）。
2. 最后一次收编同步，必须零差异。
3. （第 4 版之前）旧人工表冻结一份：`legacy_assertions`、`legacy_identity_relations` 等 5 张，外加 `legacy_identity_baseline`。**词条 schema 第 4 版（2026-09-29，拆脚手架阶段 A）起不再冻结**，已有的冻结副本在升级时删掉（第 4 节）。
4. 删掉 `assertions` 上的身份触发器（它们的语义由核心在写结论的同一段代码里执行），删掉 `shadow_*`。
5. 27 张表加写保护。
6. 按当前兼容表整片建人物 / 皮肤的 `term_projection` 镜像（S5 发现并补上，见第 6 节）。
7. 写 `meta` 的 `terms_characters=core`。

**当时的 Python 服务启动时**：`initialize` → 画师切换（`adopt_artists`）→ 补交溯源证据包 → 页面可用 → 后台 `adopt_terms`：先标签、再角色。
- 每次切换前先备份；
- 角色切换全程持有进程写锁（`Store._labels_lock`），期间写操作等待，读照常；
- 失败或中途退出都留在旧表，下次启动再试。在真实副本上用桌面 App 验证过：标签交 WD 证据中途 App 被关掉，库完好、仍是旧表，重开后接着交完并切换。

## 3. 切换之后

**写入口**：原生宿主的各业务模块（`native/terms/src/app_characters.rs`、`app_identity.rs`、`app_profiles.rs`、`app_sync.rs`、`app_index.rs` 等）把操作组装成一个 JSON 命令，在一个写事务里调用 `character_commands::command`；命令行入口是 `illustash-terms character-command`。核心在这个事务里：
- 直接写 `terms` / `term_tree` / `term_links` / `term_names`，追加 `term_verdicts` / 关系裁决；
- 执行旧的投影 SQL，维护派生表；
- 按旧文案生成历史文案，写一步 `actions` 和 `backend_effects`。

调用方只做它才看得到的部分（按筛选条件选图、联网查版权、复制品组的计算），其余校验和拒绝文案由核心按旧顺序做。命令一览、各命令同事务写的表和这些分工见方案第 8 节。

**历史与撤销**：
- 核心写的步骤是一项 `{"terms": {"v": 2, "type": …, "before": [旧形状…]}}`；历史文案按快照里的 entity id 以当前名字重新渲染。
- 撤销按 `character_commands::owns` 判断归属（`store.rs`、`native/terms/src/app_commands.rs` 都用它）：新信封，以及每一项都是角色形状（快照、`merge`、`identity_relation`、`identity_anchor`、`profile`）的旧步骤，都交角色撤销；角色形状和别的形状混在一步里，或形状不认识，拒绝，什么都不改。
- `[]`、`curation`、`identity_skip` 不是角色步骤；画师、标签步骤归各自的撤销。
- 旧历史可以按原样一步步撤到最早。

**导入与重扫**：`refresh`、`reindex`、`import`、`rederive`（派生重算）、`work_evidence` 都是核心命令。调用方用只读连接准备输入（查询和顺序照旧），候选写成证据批次（方案第 7 节）。

**学习**：研究 worker 经核心发布学习批次（`learning-snapshot` / `publish-learning`）。发布前按指纹检查快照是否过期，过期就不写、稍后重来（D6，唯一纳入的行为变化，界面看不到）。

**拒绝的操作**：激活或改身份策略。当时在副作用之前拒绝的离线工具已全部删除（#53、#186）。

**不走角色命令的**：收藏排除（`curate`）、跳过配对、名字的联网补全（名字命令，见 [TERMS_NAMES.md](TERMS_NAMES.md)）、复制品组和相似组的计算（核心只接收结果）。

## 4. 回退

- 切换前的自动备份（`library-backups/`）可以整库恢复。**切换之后做的操作会丢失**（D3）。
- 冻结的旧表不再保留（词条 schema 第 4 版，用户 2026-09-29 决定）：画师的 `artist_verdicts`、标签的 `legacy_asset_tags`、角色的 `legacy_*` 在切换时直接删掉；第 4 版之前切换的库，在下次启动升级词条表时删掉。切换前的数据只在自动备份里。
- 升级图库格式要显式进行（`http --upgrade`，见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md#已有核心库的显式升级边界)）；核心命令拒绝旧版本的库。
- 切换是单向的：切换前的旧代码读不到切换后的数据。

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
- `native/terms/tests/baseline.rs`（2026-10-19 起，#201 新对照基准，接替原 `replay.rs` 的录制，比较规则见 `tests/common/baseline.rs`）：原 Python 契约与双跑测试（`test_terms_characters_contract`、`test_terms_character_*`、`test_terms_characters_cutover`、`test_identity*`、`test_lead`、`test_web_v2`、`test_works` 等，按原模块名各一项）的场景，由 main（核心第 7 版）录下核心仍服务的调用，用当前 `illustash-terms` 重放，逐步对照输出和各表内容。夹具在 `native/terms/tests/fixtures/baseline/`；
- Rust `crates/core/src/legacy_characters/tests.rs` 等。

## 7. 体积与性能

真实规模（2026-09-29 切换时的测量，早于两轮性能优化；现状见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)）：

- **首次启动的切换**约 2 分钟：画师 2 秒、交包 3 秒、标签 107–134 秒（在后台；桌面 App 里与其他后台任务一起跑时 378 秒）、角色 13–27 秒。
- **库**：451.5 MB → 1,098.6 MB，主要是标签那一步的 257 万条 WD 证据；切换期间 WAL 最大约 262 MB，另外每次切换前各有一份自动备份。
- **日常裁决、sweep、回答**与旧版相当。关系决议、合并约为旧版 2 倍；全量 refresh 约 3.4 倍（57 秒）；新建角色 2.6 倍；reindex / 导入约 2.3 倍；撤销合计 1.6 倍。
- `links` 的关键查询计划与旧版相同。

## 8. 之后的进展与没有做的

- 名字：已收编（[TERMS_NAMES.md](TERMS_NAMES.md)）。Windows：2026-10-01 已验收（#72）。
- 删旧表：冻结表第 4 版删掉；兼容表在核心第 8 版删掉或定为派生表（#201，[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节）。
- 身份策略 v2：第 7 版放弃（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 12 节）。
- 性能待优化清单见性能文档。

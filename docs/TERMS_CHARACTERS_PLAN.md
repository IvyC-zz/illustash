# 词条第 4、5 步方案：角色、皮肤 / 变体、作品

状态：已完成的方案，留作代码注释按节号引用的设计记录，2026-11-12 精简。现行行为、映射、切换经过、与旧行为的差别和验收见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md)，schema 见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 7、8、13、14 节，性能见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)。本文只留别处没有、仍然有效的设计和用户决定。方案原来让旧表成为核心维护的兼容表，这部分已被词条核心第 8 版（#201，2026-10-20）取代：兼容表删除或定为核心维护的派生表，收编与切换命令删除。

2026-09-28 起草并经用户确认（第 14 节），2026-09-29 完成。节号沿用原方案，删掉的节（审核逐项回应、逐表映射、名字边界、性能测量、待核实项）不重新编号，原文在 git 历史里。代码注释里的 S0–S6 指第 12 节的分段，D1–D6 指第 14 节的决定。

## 0. 原则

1. **等价优先，规则原样。** 只换数据的归属和写入方，不改规则。归属的 SQL 规则（`membership_rules`、`family_confirmed`、`effective_assertions`、`variant_memberships`、`identity_assignments`、`ambiguous_claims`、`family_evidence` 的插入语句、skey 的 `printf`、折叠 `MIXED`、收录规则）原文不动，逐字保存在 `crates/core/src/character_sql/`，由核心在自己的事务里执行；非 SQL 的规则（身份图、作品证据、候选解析、新身份的层级名字规则、快照撤销）逐行移植到 Rust。按词条模型重新定义语义是之后单独的一步，要有自己的等价验收（第 11 节）。
2. **按命令归属，不按表名归属。** 每个改变角色、作品状态的业务命令都是一个核心命令、一个核心事务；它需要的外围表在同一事务里由核心一起写，表的形状不变（第 8 节）。
3. **没有 action 的人工状态不伪造历史。** 收编基线、相册导入和撤销审计记在非用户批次里，用排序锚点放进时间线；它们不进撤销栈，旧历史的每一步仍按原来的方式连续撤销（第 6 节）。
4. **可替换的机器结果按有效批次生效。** 证据只增不删；角色候选按「最近一次成功批次覆盖的图」取当前有效证据，成功的空结果让旧证据失效。学习产物是判定器私有状态，整批替换（第 7 节）。

## 2. 核心 schema v2

schema v1 已随第 2、3 步用于切换，不能改；新增一次迁移 v2，单事务。落地后的表见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 7 节。

| 变更 | 内容 | 对第 2、3 步的影响 |
|---|---|---|
| 非用户批次 `term_batches` | `id, kind(baseline / album / undo_audit / migration), anchor, label, data, created_at`。`anchor` 是创建时「最近一步 action 的 id」，基线则是计算出的位置 | 无 |
| `term_verdicts` 扩展 | `action_id` 可空、新增 `batch_id`（两者恰好一个），`anchor`（action 裁决 = action_id；批次裁决 = 批次的 anchor），`via`（原 origin 细分：review / sweep / album / merge / migration / undo） | `term_effective_verdicts` 与 `store.rs::project` 的 `JOIN actions` 改成：action 未撤销或属于批次；排序改为 `anchor DESC, batch_id IS NOT NULL DESC, id DESC`。画师、标签的裁决都是 action 裁决，结果不变 |
| 关系裁决的完整序列 | 新视图 `term_relation_latest`：每条关系最新一条（**含 clear**），给「未复核继承层级」用 | 无 |
| 证据批次 | `term_evidence_sets(id, producer, source, method, scope all/assets, created_at, package_id)`、`term_evidence_set_assets`、`term_evidence_set_rows(set_id, evidence_id)`、有效集 `term_active_evidence`（可由前三张重建） | 只对标为「可替换」的来源生效（`wd.wd_character`、`ccip_index.char_ext`）；题材、分级、画师照旧全部有效 |
| 私有状态表 | `term_identity_baseline`（= `identity_baseline`，活表）、`term_tree`（作品上级、锁、角色 work_id、`work` 文字）、`term_legacy_ids`（旧 `identity_relations.id`、`identity_decisions.id` ↔ 核心 id） | 无 |
| 写保护 | 核心事务开始时在 `meta` 写 `terms_writer`，提交前删除；受保护表的 INSERT/UPDATE/DELETE 触发器在该键不存在时 `RAISE`（别的连接看不到未提交的行） | 无 |

## 5. 既有语义（逐行移植）

### 5.1 身份图（hybrid）

1. 每次重建前先追加基线：还不在 `term_identity_baseline` 的身份，按当时的 parent_id / variant / kind / merged_into 追加。新身份的层级（名字规则）只在这里，不是关系，人工不能覆盖（原行为，第 11 节）。
2. 建图：
   - 基线行按 `seq`（原 rowid）顺序；先按基线 `merged_into` 合并，再按 accepted 的 `duplicate_of`（**按关系 id 顺序**）合并，`join(a,b)` 的根取 b 那一侧；
   - 再在等价组上挂层级：先挂基线父，后挂 accepted 的 `form_of`（按 id）；
   - 出错条件：变体与其人物重复、变体已有另一个父人物、层级成环、`different_from` 与等价组或层级冲突；
   - 不限制同层合并（独立人物的别名可以并进已有的皮肤组）。
3. 投影：
   - 先按 scopes 判断等价组是否真拆开，拆开的挂起；认领被拆开的改成待定；
   - 重写 canonical，逐行写回身份结构；
   - `graph_version` = `json.dumps([rows, sorted(pending)])` 的 sha256，按 Python 默认（`ensure_ascii`、`', '` 分隔）逐字节复刻；
   - 版本变了：失效校准、清认领；对受影响的图（`affected_images` 加挂起的图）重算证据与归属。
4. `accept-many` 逐条接受、逐条校验，最后重建一次。`decide` 每次都追加一条决议，并记下当时关系上全部证据的指纹，包括退回 proposed。
5. 「未复核继承层级」：关系上有 `legacy-hierarchy` 证据，且**最近一条裁决（含 clear）**的 reviewed 没有包含它。
6. 原来 `assertions` 上的触发器语义：写一条结论时，删掉该 (图, 身份) 的 scope / suspended；所在等价组多于一个成员时，按组成员重写 scope。由核心在写裁决的同一段代码里执行，别处不再触发。

### 5.2 作品树（状态，不是重算）

- `create`：新作品一律上锁（有无上级都锁）。`move`：先查不成环（子树包含目标就拒绝），然后上锁。`link`：旧的 Danbooru 链接改为非当前、新链接为当前，status 改 confirmed、note 清空。清空链接：status = confirmed，note = 「外部链接已由用户清空」。
- 作品证据 `work_evidence`（原 `works.apply_evidence`）：
  - 遍历 qualifier 的顺序 = 原查询的行序；
  - 按 tag 排序处理上级候选；
  - 已上锁的跳过；多个候选只写 note、**保留已有上级**；成环只写 note；只有一个时直接覆盖未上锁的上级。
- 两个反例：已挂 B 的 A 收到 B、C 两个上级 → 仍是 A→B；A、B 互为上级 → 只接受先处理的 A→B。
- 子树计数用递归查询 `TREE`；不给作品建逐图投影。

### 5.3 归属规则

`family_evidence` 的插入语句、`membership_rules`、`variant_memberships`、`ambiguous_claims` 按当前策略的原 SQL 执行。由此自动保持：先排名后筛否定、各项条件、hybrid 皮肤视图读代表的原始结论、相册确认不能单独确认皮肤、skey 格式。

## 6. 历史与撤销

### 6.2 人工结论的批次与锚点

收编时，每个 (图, 身份) 的人工结论按未撤销步骤的事件序列重建：有步骤的裁决挂在那一步上；初值、中途出现的无 action 写入（例如前一步撤销后又被相册导入写入）、没有任何事件的当前行，分别进非用户批次，锚点取对应事件 id − 1 或 0；无法解释的计数并阻止切换，不归给最近的 action。身份关系同样处理：有 action 的决议挂 action，`activate` 写的决议挂 `migration` 批次（没有 reviewed，所以继承层级仍是「未复核」），旧 id 经 `term_legacy_ids` 对应。

这样有效裁决按锚点排序回到上一条时，正好是旧版撤销恢复的 before，旧历史可以一步步撤到最早。相册导入只在「没有有效结论」时写入 album 批次，不改成覆盖。收编代码已随 #201 删除，批次和锚点留在数据里。

### 6.3 撤销适配器

归属规则见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 第 3 节「历史与撤销」（`character_commands::owns`）。各形状由核心这样撤：

| 形状 | 核心的撤销 |
|---|---|
| 快照 `{uuid, entity, assertion, deferred[, identity]}`（entity 可空） | 标记撤销，有效裁决随之回退；`deferred` 按原值恢复；有 `identity` 就恢复 scope / suspended / uncertain；重算这些图的归属与折叠；删 `asked` 中该 action 的记录 |
| `merge`，策略未设置 | 标记撤销：裁决上的 clear / yes / no 失效，`merged_into`、候选、皮肤父、别名、pins、reviews 按快照恢复 |
| `merge`，hybrid（激活前留下的旧合并） | 在 `undo_audit` 批次追加对应 `duplicate_of` 的 clear（reason「撤销历史合并；原归属无法区分的认领待确认」，reviewed = 当时的全部证据），再重建图；找不到迁移记录时拒绝 |
| `identity_relation` | 标记撤销，并在 `undo_audit` 批次追加 previous_status 对应的裁决（reason「撤销人工关系决议」，reviewed = 当时的全部证据），因此「未复核」的判定与旧版一致；再重建图 |
| `identity_anchor` | 恢复原锚点结论、追加历史 `undo`、失效校准、清认领、重算归属；不恢复旧的机器认领 |
| `profile` | 恢复用户名字、Danbooru 链接、`work`/`work_id` 及其皮肤的 `work`；链接变了就重建图 |
| `artist_verdict`、`{uuid, tag, had}`、`terms` | 与第 2、3 步相同 |

- 都要做的：删掉该 action 的 `reviews`；核对最近一步。
- 挂起只在等价组**真的拆开**时触发。
- 新步骤写成 `[{"terms": {"v": 2, "type": …, …}}]`；零成员的 `sweep` 也是非空数组，仍写 `reviews`。
- 核心步骤里保留快照的 entity id，历史文案按当前名字重新渲染时与旧版相同。

## 7. 证据批次

- **可替换来源**：`wd.wd_character`（source.sqlite `labels` dim=character、conf ≥ 0.35）和 `ccip_index.char_ext`（dim=char_ext）。每个批次写明覆盖范围：`refresh` 全库；`reindex` 被改动的图（含索引 drop）；`import` 新图与移除的图。
- **生效规则**：覆盖范围内每张图的当前有效证据 = 本批次的行；覆盖到但没有行（成功的空结果）= 该图这个来源的证据清空。失败的批次整事务回滚，旧的有效集不动。
- **版本**：按行取内容摘要；同样的结果复用原证据行，内容不同就另起一行，不再冲突。
- **解析**：index `wd_tag` 唯一时用它，否则依次找 `wd` 链接、`danbooru` 链接，再没有就新建带 `wd` 链接的身份；CCIP 找 `ccip` 链接，没有就新建。解析和建词条在同一个核心事务里，新身份的 UUID 由核心生成。同一 (图, 原始词条, 来源) 的多条标签先聚合（取最大分，标签值记进 data），再入证据表。
- **索引数据**：角色、层级、tag、别名、默认标签合成一个「索引同步」，输入是调用方从 source.sqlite 只读导出的 `characters` / `character_aliases` 行。`wd_tag` 不唯一，所以不当链接存，只在解析时使用。
- **学习批次**：调用方算原型，发布时一次交核心：`prototypes`、`learn_state`、`proto_claims`、`weak_claims` 整批，以及变化的图。核心整批替换，并对变化的图重算归属与折叠；过期的结果拒绝（D6，第 16 节）。

## 8. 写入口与事务归属

所有角色业务命令都经 `illustash-terms character-command --db <库>`，标准输入给一个 JSON 命令 `{"op": …}`，一个核心事务；`effect` 给出时同事务写 `backend_effects`。「同事务写」指核心在同一事务里一起写的外围或私有表。

| 入口 | 进历史 | 核心命令 | 同事务写 | effects |
|---|---|---|---|---|
| `change` | 是 | `change` | deferred、identity_uncertain、折叠 | verdict |
| `answer`、`answer_audit` | 是 | `answer`、`answer_audit` | deferred、identity_uncertain、asked、折叠 | verdict |
| `reassign`、`correct` | 是 | `reassign`（核心内复查「原角色归属已变化」） | deferred、identity_uncertain、折叠 | verdict |
| `sweep` | 是 | `sweep` | reviews、折叠 | verdict |
| `merge` | 是 | hybrid：一条 `duplicate_of` 决议；策略未设置：破坏式合并 | 别名、prefs.pins、reviews、折叠 | verdict |
| `decide_identity`、`uncertain_identity` | 是 | `propose` / `decide` / `accept-many` / `rebuild`、`uncertain` | 私有表、meta、折叠 | verdict |
| `correct_identity_anchor` | 是 | `anchor` | 锚点表、learn_state、meta | 无 |
| `set_profile`、`link_local` | 是 | `set_profile`、`link_local`（版权由调用方在核心事务外联网取得） | 名字、作品复核、图重建 | verdict |
| `create_entity`、`create_linked`、`shared_oc` | 否 | 结构命令，不进历史 | 名字；hybrid 下重建图 | 无 |
| `set_kind`、`set_mascot`、`set_work`、`edit_work`（create / edit / from_tag / move / link / rename） | 否 | 结构命令，不进历史 | 名字、作品复核、受影响图的归属 | 无 |
| `import_changes`（同步） | 否 | `import`，一个事务（第 9 节） | assets、album 批次、imported、相册别名、duplicates、折叠、auto_rules、excluded、sync_added、meta.indexed | imported |
| `refresh`、`reindex_characters` | 否 | `refresh` / `reindex`，附索引同步 | 同上 | 无 |
| WD 重交后重算派生 | 否 | `rederive`：重算 single / lead / 归属 / 规则 / 排除 | 同上 | 无 |
| 默认只当标签的名单 | 否 | `default_tags` | — | 无 |
| 作品证据 | 否 | `work_evidence` | 作品树 | 无 |
| 学习发布 | 否 | `learning-snapshot` / `publish-learning` | 私有表、折叠 | 无 |
| `undo` | — | `undo`，按第 6.3 节分派 | — | verdict |
| `activate_identity` | 否 | 拒绝：改策略是迁移，不是日常写 | — | — |

跳过配对、置顶、收藏排除不走角色命令。新增行为「作品和类型编辑可撤销」不在本步（第 11 节），这些接口的 HTTP 响应形状不变。

**模块**：

| 模块 | 命令（op） |
|---|---|
| `character_commands.rs` | `change`、`answer`、`answer_audit`、`reassign`、`sweep`、`uncertain`、`anchor`、统一撤销 `undo`；身份关系的 `propose`、`decide`、`accept-many`、`rebuild` 转给 `character_identity.rs` |
| `character_merge.rs` | `merge` 及策略未设置时的撤销 |
| `character_profile.rs` | `set_profile`、`link_local` 及 `profile` 撤销 |
| `character_structure.rs` | `create_entity`、`create_linked`、`shared_oc`、`set_kind`、`set_mascot`、`set_work`、`edit_work`、`work_evidence` |
| `character_import.rs` | `refresh`、`reindex`、`import`、`rederive`、`default_tags` |
| `character_evidence.rs` | 候选写成证据批次（第 7 节） |
| `character_names.rs` | 按旧 `put_name` 语义写用户名字和别名：display 只接受有语言、来源在优先级表里的名字，来源为 danbooru 的 display 丢弃；设首选时先清掉同语言其他名字的首选；upsert 时首选取较大值 |

**命令里定下的做法**：

1. **人工结论追加成 `term_verdicts`**（挂在这一步上，它是权威），派生表 `assertions` = 每个目标的有效裁决。身份、结构、链接、名字、作品复核第 8 版起直接写 `terms` / `term_tree` / `term_links` / `term_names`。候选只来自证据批次：派生表 `candidates` 按图由有效证据算出。
2. **hybrid 的 scope 维护**在写结论的同一段代码里执行（`scope_on_insert` / `scope_on_delete`，含「有行才触发删除」的细节，第 5.1 节第 6 条）。
3. **调用方给的**：按筛选条件算出的选择集（算不出时交来旧文案 `asked_refused`，核心在原来算选择集的那一步报出，检查先后不变）、作品和 tag 的可读名、联网查到的版权、复制品组的全量分组。**核心在自己的事务里重做的**：选择集的复制品展开、目标身份解析、原归属、tag 归属、作品查找、成环、上锁、父角色是否被移出、最近一步；历史文案也由核心生成（`character_labels.rs`，[TERMS_NAMES_PLAN.md](TERMS_NAMES_PLAN.md) N5）。
4. **新身份、新作品的 id** 可以由调用方给；命令里写 `false` 时，核心在真要新建的那一刻以 `Error::NeedId`（退出码 5，标准错误是这个 id 在命令里的 JSON 指针）要一个，什么都不写，调用方补上后重来；不给就由核心生成 UUID。
5. **策略未设置时，`candidates` 沿破坏式合并解析到代表**。撤销合并时，按旧版对候选的改法算出这几张图应有的行；与证据算出的不同（合并发生在收编之前），就为这几张图另起一个证据批次（`undo-merge/<步骤>`）。
6. **相册导入**进 album 批次，锚点 = 当时 `actions` 里最大的 id；`INSERT OR IGNORE` 真的写了才追加裁决。

## 9. 事务边界

1. **一个业务命令 = 一个核心事务**：调用方用只读事务解析 id 和输入后交核心；核心在自己的 `BEGIN IMMEDIATE` 里重新核验：图存在、复制品组展开（在核心内按 `duplicates` 重算，不信任调用方）、目标身份解析、原归属、父角色是否被移出、最近一步。
2. **后处理**：核心同事务写 `backend_effects(kind)`，调用方提交后照旧处理，学习与相似扫描照常被唤醒。
3. **折叠**：成员有变化的同一事务里，核心对受影响的组执行原来的折叠 SQL。
4. **导入**：调用方只读 source.sqlite，准备好新图行、删除列表、角色标签行、相册决定与映射、`duplicates` 全量分组（它不依赖成员）、`realistic` 列表；核心 `import` 一个事务按旧顺序提交：assets → 候选批次 → 相册批次 + imported + 别名 → 证据与单人、线索 → 归属 → duplicates 替换 → 折叠 → auto_rules → excluded → sync_added → meta.indexed → effects。核心失败就什么都没提交，调用方不再做后续写入。
5. **恢复**：所有写入都在核心事务里，没有核心写过而调用方还没完成的中间态；`backend_effects` 是可重放的待办记录。

## 11. 推迟的行为变化

本步只做等价迁移（D6），下面这些都没做：

| 变化 | 原因或来源 | 决定 |
|---|---|---|
| 作品新建 / 改名 / 移动 / 链接、角色归作品、设为原创、当作标签变成可撤销的步骤 | TERMS.md「裁决带 action_id」 | 延后；要改撤销栈，并定下前端怎么显示 |
| mascot 改成 `kind=label` | TERMS.md「只当标签只是改 kind」 | 延后；现在的 `set_kind` 会写一步 action，与「不进历史」矛盾 |
| 作品树改成「由证据重算」、不依赖处理顺序 | 词条模型 | 延后；第 5.2 节的两个反例结果会变 |
| `same_as` 只允许同层 | 原方案 | 撤回；列为之后的规则变更 |
| 身份基线边可以被人工覆盖 | 词条模型「裁决先于证据」 | 延后 |
| 学习批次发布时拒绝过期结果 | 方案审核 | **纳入**（D6），界面看不到 |
| 相册导入进撤销栈 | 原方案 | 撤回；保持不进 |

## 12. 分段

代码注释里的段号：

| 段 | 内容 |
|---|---|
| S0 | 在旧代码上写夹具钉住现有行为（作品反例、跨层合并、相册与撤销、旧合并在 hybrid 下撤销、零成员 sweep、混合与未知形状的步骤、空的成功批次、索引 drop） |
| S1 | schema v2（第 2 节） |
| S2 | 影子收编 `sync-characters`（已随 #201 删除） |
| S3 | 核心命令与统一撤销（第 5–8 节），旧新双跑 |
| S4 | 切换与接线 |
| S5 | 真实副本验收 |
| S6 | 文档 |

结果见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 第 6 节。

## 14. 用户确认的决定

**2026-09-28 用户确认：D1–D6 全部按下表执行。**

| # | 决定 | 内容 | 理由 |
|---|---|---|---|
| D1 | 支持哪些身份策略 | 支持未设置和 hybrid；v2 的库拒绝切换。v2 之后在第 7 版放弃（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 12 节） | 两者的规则都是现成的 SQL，可以原样执行；v2 的校准门槛未达，没有真实使用 |
| D2 | 撤销边界与基线 | 用非用户批次加排序锚点；旧历史的每一步都能按原样连续撤销；基线不进撤销栈、不能撤；出处无法解释的计数为 0 才能切换，否则逐条给用户看；相册导入切换后仍不进撤销栈 | 不伪造历史，也不挡住撤销栈 |
| D3 | 切换方式与回退 | 回退 = 恢复切换前的自动备份，**切换之后做的操作会丢失** | 做不到无损回退，所以明确说出来 |
| D4 | 名字是否本步收编 | 不收编，核心命令只同事务写用户名字和别名；名字作下一步，已完成（[TERMS_NAMES.md](TERMS_NAMES.md)） | 角色判定和作品树都不依赖名字，拆出去能保住本步的等价边界 |
| D5 | 派生表形态 | 派生表由核心写、加写保护；规则视图原样保留、由核心执行同一段 SQL。兼容表部分已被第 8 版取代 | 等价由构造保证；漏改的写入会直接报错，不会悄悄成功 |
| D6 | 行为变化 | 第 11 节全部延后，只纳入「学习批次拒绝过期结果」 | 过期学习结果本来就会在下次学习时纠正，拒绝只是提前重来 |

## 16. 已批准保留的旧行为

**D6 的过期判断**：`learning-snapshot` 在只读事务里，按固定顺序对学习依赖生成带版本的 SHA-256 指纹：资产、实体、链接、人工结论和完整 actions（含 undone）、候选、单人/线索、重复组和图片关系、身份基线/关系/决议/人工私有状态、当前学习四表，以及身份策略/图版本/校准/索引相关 meta。按列类型与长度编码，BLOB 原字节参与；不依赖行数、最大 action id 或可复用的 rowid。`publish-learning` 在 `BEGIN IMMEDIATE` 内重新算指纹，不相等返回 `status=stale`，什么都不写，也不更新 `learn_state`；两次快照之间有任何变化都保守拒绝，调用方稍后按新快照重来，对外状态不变。

下面两处是旧版本来的不一致，用户批准原样保留（[TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 第 5 节），增量与全量一致的验收只在这两处例外，各有单独的测试钉住：

1. **≤ 1e-6 的分数变化不刷新归属**：学习发布时，只有新增/消失的认领，或同一 `(uuid, entity)` 的分数比前一批变化 **> 1e-6**，才刷新该图的派生表；≤ 1e-6 时新分数照写 `proto_claims`，原派生值保留。这会让全量重算与增量结果不同：skey 的 `%020.12f` 能反映 5e-7 的变化；单人图上两个人物分数接近时，归属本身也可能翻转（行集合变化）。人物/皮肤的 `term_projection` 镜像也照增量结果。取消阈值留给以后的规则变更。
2. **hybrid 下撤销身份合并**：重算范围只来自变动身份的 candidates、proto_claims、assertions 和 identity_scopes 的拆组图片，identity_claims 拆组新增的未定图片没有补进去；全量执行原 SQL 时这些图的原归属会被 identity_uncertain 排除，增量里仍留着。

**画师证据的整合**（2026-09-29 合入找回画师，schema 第 3 版，[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 8 节）：

- 写 `image_artists.status` 的规则由方法决定：`auto` 是 pixel_hash、gallery_match、iqdb、same_picture；`candidate` 是 ocr 和 group_*。核心画师判定器的强证据与 `auto` 相同，其余方法只到「也许是 TA」。
- 读路径：`for_asset` 返回 `maybe`（投影为「也许是」、没人判断过的画师，原因取当前有效证据里分数最高的一条），`methods` 列出当前有效证据的全部方法，抽查池只看当前有效的强证据。
- 用户已确认的候选，分组推荐重算后不再提出时，人工的「是」继续有效：机器证据更新不抹掉人工裁决（裁决先于证据）。

# 词条第 4、5 步修订方案：角色、皮肤 / 变体、作品、名字

状态：已完成的方案，留作代码注释按节号引用的设计记录。其中「旧表成为由核心维护的兼容表」的部分已被词条核心第 8 版（#201，2026-10-20）取代：兼容表删除或定为核心维护的派生表，收编与切换命令删除，现状见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13、14 节。

2026-09-28，已确认（第 14 节）。回应方案审核 `TERMS_CHARACTERS_REVIEW.md`（审核基线 `fbee1f0`，审核提交 `aeb42c0`；审核稿与交接文档已删除，见开源前私有历史）。开篇方案记录于 2026-09-28；后续实现进度见文末，当前恢复入口见下节。原审核行号指 `aeb42c0`（业务代码与 `fbee1f0` 相同）。Python 测试 2026-10-07 起已删除（#9）：文中提到的 `tests/test_*.py` 改由 `native/terms/tests/replay.rs` 中同名模块的录制重放及 Rust 单元测试覆盖，现状见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 第 6 节与 [TERMS_NAMES.md](TERMS_NAMES.md) 第 6 节。

## 现状（2026-09-29）：S0–S6 全部完成

结果说明见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md)。性能现状见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)。回合 main 由用户另行决定。下面的「当前恢复入口」是 S4 中断时写的，保留作历史。

## 当前恢复入口（2026-09-29，已完成，保留作历史）：先整合 main，再完成 S4

**状态：S4 中途暂停，未完成。** 用户说明 Claude 因额度中断；这不代表 S4 已验收。代码检查基线为 `terms-core@328d742`、`main@954ce64`，两边工作区当时干净，分叉后分别有 22、19 个独有提交。本次只更新文档并推送；没有执行分支合并、修改业务代码、重跑测试或访问真实图库。

**后续顺序：main → terms-core 溯源整合 → 完成 S4 → S5 副本验收 → 单独决定是否回合 main。** 在 terms-core worktree 吸收 main 今天已敲定的溯源行为，再继续迁移验收，避免围绕旧基线重复验收。main 主目录保持不动；禁止把未验收的 terms-core 提前合回 main。实际合并前重新检查两边 HEAD、工作区和远端，记录选定的 main 提交，保留现有 S4 工作，不重置分支、不为整合重写已有历史。

### 整合范围与验收门槛（待执行）

只读三方合并预览在 `tests/reference/web/artists.py（#9 已删除）` 出现文本冲突；自动合并成功也不能代替业务验收。以 main 的最新溯源产品行为和 terms-core 的核心写入、裁决及撤销边界共同作为目标，不能整文件选一侧覆盖另一侧。

1. **新证据与候选读路径。** 保留 IQDB、`same_picture` 自动归属，`group_oc` / `group_cluster` / `group_run` 候选及原因；核对核心判定器、证据方法映射、`term_projection` 和 `tests/reference/web/artists.py（#9 已删除）`。当前核心读路径 `_core_for_asset()` 未返回 `maybe`，`STRONG` 仍仅列 `pixel_hash`、`gallery_match`。验收旧路径与切换后路径的归属、候选原因、数量、人工确认 / 否定 / 撤销一致；候选确认前不能计为正式归属。新方法的强弱遵循 main 已定行为，不顺带重设计规则。
2. **候选失效与重复交付。** main 的 groups 步骤每次删除旧分组候选再重算；`tests/reference/provenance/export.py（#9 已删除）` 交全量快照、核心保留历史证据。核实并补齐有效范围 / 失效机制，不能假定快照中缺失的候选自动失效。测试候选消失、成功空结果、重复交付、失败 / 取消后的部分交付，区分成功撤回与未完成计算；机器证据更新不得抹掉人工裁决。
3. **运行生命周期和权限。** 保留 main 的便宜后台步骤、选图查找、指定画师查找、后台暂停恢复，以及未授权只跳过联网步骤、本地步骤继续执行。核对 `tests/reference/web/provenance_job.py（#9 已删除）` 的交包与 `tests/reference/web/server.py（#9 已删除）` 的接线：正常结束、取消、失败、重启补交后核心结果和页面刷新一致，按需运行结束后后台正确恢复；离线回归使用假网络，不以本次文档授权开展外部请求。
4. **角色兼容与完整回归。** 分组推荐读取 `duplicates`、`similar_groups`、`memberships`、`entities`、`clusters`，核对切换后的兼容读路径。先补溯源整合测试并跑两边相关回归，再完成文末 S4 未做项和完整 Rust / Python 回归。S5 仍需逐次授权，仅在副本上做当前状态、连续撤销、增量 / 全量重建和 App 读路径对照；历史测试数字不代表合并后通过。

本节是恢复后的优先执行顺序；下文 S0–S4 记录保留为历史证据，历史记录中的“未完成”“不进行下一阶段”按对应时间理解。S4 原剩余项目仍全部有效，溯源整合通过不能代替 S4 完成。

## 0. 修订后的六条原则

1. **等价优先，规则原样。** 这一步只换数据的归属和写入方，不改规则。现有的 SQL 规则（`membership_rules`、`family_confirmed`、`effective_assertions`、`variant_memberships`、`identity_assignments`、`ambiguous_claims`、`family_evidence` 的插入语句、skey 的 `printf`、折叠 `MIXED`、收录规则）**原文不动**，改由核心在自己的事务里执行；Python 写的非 SQL 规则（`identity._graph` / `_project`、`works.apply_evidence`、`_candidate` 的解析、`_hierarchy` 的名字规则、快照撤销）逐行移植到 Rust。把规则重写成 Rust 代码、按词条模型重新定义语义，是之后单独的一步，要有自己的等价验收。
2. **按命令归属，不按表名归属。** 切换后每个改变角色、作品状态的业务命令都是**一个核心命令、一个核心事务**。它需要的外围表（`deferred`、`asked`、`reviews`、`imported`、`names`、`prefs.pins`、`duplicates`、`auto_rules`、`excluded`、`backend_effects`、`meta` 的相关键）在同一事务里由核心一起写；表的形状不变。
3. **人工主数据进词条表，读的对象保持同名同形。** 裁决、关系、词条、链接、证据的主数据在 `term_*`；Python 读的 `entities`、`assertions`、`candidates`、`memberships` 等是由核心维护的**同名同形兼容表**（热表不用视图），切换后 Python 写它们会被触发器拒绝。
4. **没有 action 的人工状态不伪造历史。** 用独立的「非用户批次」记录收编基线、相册导入和撤销审计，用排序锚点放进时间线；它们不进撤销栈，旧历史的每一步仍按原来的方式连续撤销。
5. **可替换的机器结果按有效批次生效。** 证据表仍只增不删；角色候选按「最近一次成功批次覆盖的图」取当前有效证据，成功的空结果会让旧证据失效。学习产物是判定器私有状态，整批替换。
6. **名字这一步不收编。** 名字表和选名规则留在 Python；核心命令只在同一事务里写入用户名字和别名。名字投影单独作下一步，前置契约见第 10 节。

## 1. 逐项回应

| 项 | 结论 | 回应 / 修订位置 |
|---|---|---|
| 事实 3 | 接受更正 | `change`、`reassign` 不写 `asked`；`answer`、`answer_audit` 写；`sweep` 写 `reviews`（`tests/reference/web/store.py（#9 已删除）:1877–1936,2015–2150`）。矩阵按此写（第 8 节）。 |
| 事实 8 | 接受更正 | `learn` 没有 dirty 且不 `reclaim` 时提前返回（`web/learn.py（#9 已删除）:156–157`）；只有进入 `_claim` 才整表替换。 |
| B1 历史重建 | 接受 | 按事件类型分别重建，基线用非用户批次定位；区分当前状态等价与连续撤销等价；不能解释的出处计数并阻止切换（第 6 节）。album 批次保持「没有结论才写」，不改成覆盖。 |
| B2 proposed 与回落 | 接受（机制不同于「hold」） | 身份关系**没有机器回落**：图只采用「人工 yes 或非用户批次 yes」的关系，以及 `identity_baseline` 里的基线边（与 `_graph` 相同，`web/identity.py（#9 已删除）:298–333`）。人工退回 proposed = 追加一条 `clear`，它是最新的一条，所以不会回落。`reviewed` 按每条裁决保存，包括 `clear`，「未复核继承层级」按最近一条裁决计算（第 5.1 节）。`identity_baseline` 作为活的私有表保留，新身份照旧追加。迁移证据用 `migration-hierarchy` 等来源，不用被禁止的 `legacy`（`crates/core/src/schema.rs:188`）。 |
| B3 有效批次 | 接受 | 新增证据批次与有效证据集，定义覆盖范围、成功空结果、失败（第 7 节）。初次收编以现有 `candidates` 整表作为迁移批次。「角色证据交包可以先做」撤回：交包只在切换后、由核心在同一事务里解析并建词条。 |
| B4 作品树与身份图语义 | 接受 | 作品上级是**状态**，由移植后的 `apply_evidence`、`move`、`create` 按原顺序更新，不是从证据重算；身份图逐行移植 `_graph`/`_project`，保留 b 为代表、处理顺序、先等价后层级、不限制同层（第 5 节）。四个反例进夹具。改语义的列进第 11 节，不混入。 |
| B5 名字 | 接受；改为这一步不收编 | 名字表与规则留 Python，核心只按 `put_name` 的原语义写入（第 10 节）。名字包、词表、`View` 直读路径、投影失效依赖作为下一步的前置契约列出。 |
| B6 跨连接原子性 | 接受 | 进程级写锁、单事务核心命令、核心内重新核验、导入全部在一个核心事务提交、切换在写锁下后台进行（第 9 节）。 |
| B7 后处理与折叠 | 接受 | 核心在同一事务写 `backend_effects`（`kind` 由调用方给出）、按 `MIXED` 刷新 `duplicates.hidden`；导入命令内按原顺序执行 `auto_rules`、`excluded`（第 9 节）。 |
| I1 字段与辅助表 | 接受 | 15 列逐列映射、预检、`work` 原样保存、`work_review` 三列保真、`assertions` 保存原 origin（第 4 节）。mascot 这一步不改 kind，所以 `links` 视图不会漏掉它们。 |
| I2 规则视图 | 接受 | 这些视图全部原样保留，建在兼容表上，核心执行同一段 SQL（原则 1）。 |
| I3 hybrid 契约 | 接受 | 规则 SQL 原文执行，等价由构造保证；对照仍比较全部列（含 `origin`、`sources`、`score`、`source_count`、`ccip_only`、`skey`、`rank`、`wd`、`ccip`）。 |
| I4 撤销适配 | 接受 | 按完整形状识别，旧步骤用原 `before_json` 恢复外围与私有状态；新步骤带版本；空数组 `[]` 与 `curation`、`identity_skip` 仍归 Python；未知或混合形状拒绝（第 6.3 节）。交接文档第 4 节「`entity` 步骤」的说法有误：`entity` 是快照里的字段，以本文为准。 |
| I5 写入口、工具、测试 | 接受 | 写入口矩阵见第 8 节；工具在任何副作用前检查切换标记；测试分旧态夹具、迁移、切换后命令三层（第 12 节）。 |

没有不接受的项。

## 2. 核心 schema v2（方案，不含代码）

schema v1 已随第 2、3 步用于切换，不能改；新增一次迁移 v2，单事务。

| 变更 | 内容 | 对第 2、3 步的影响 |
|---|---|---|
| 非用户批次 `term_batches` | `id, kind(baseline / album / undo_audit / migration), anchor, label, data, created_at`。`anchor` 是创建时「最近一步 action 的 id」，基线则是计算出的位置 | 无 |
| `term_verdicts` 扩展 | `action_id` 可空、新增 `batch_id`（两者恰好一个），`anchor`（action 裁决 = action_id；批次裁决 = 批次的 anchor），`via`（原 origin 细分：review / sweep / album / merge / migration / undo） | `term_effective_verdicts` 与 `store.rs::project` 的 `JOIN actions` 改成：action 未撤销或属于批次；排序改为 `anchor DESC, batch_id IS NOT NULL DESC, id DESC`。画师、标签的裁决都是 action 裁决，结果不变，由现有测试核对 |
| 关系裁决的完整序列 | 新视图 `term_relation_latest`：每条关系最新一条（**含 clear**），给「未复核继承层级」用 | 无 |
| 证据批次 | `term_evidence_sets(id, producer, source, method, scope all/assets, created_at, package_id)`、`term_evidence_set_assets`、`term_evidence_set_rows(set_id, evidence_id)`、有效集 `term_active_evidence`（可由前三张重建） | 只对标为「可替换」的来源生效（`wd.wd_character`、`ccip_index.char_ext`）；题材、分级、画师照旧全部有效 |
| 私有状态表 | `term_identity_baseline`（= `identity_baseline`，活表）、`term_tree`（作品上级、锁、角色 work_id、`work` 文字）、`term_legacy_ids`（旧 `identity_relations.id`、`identity_decisions.id` ↔ 核心 id） | 无 |
| 写保护 | 核心事务开始时在 `meta` 写 `terms_writer`，提交前删除；兼容表和私有表的 INSERT/UPDATE/DELETE 触发器在该键不存在时 `RAISE`（别的连接看不到未提交的行） | 无 |

## 3. 身份策略的支持范围

- 支持 `identity_policy` 未设置（新图库的默认，只有 `tools/identity_rollout.py`、`tools/eval_bench.py`（两者 2026-10-05 删除，#186）会激活，`tests/reference/web/store.py（#9 已删除）:2153`）和 `hybrid`。两者的规则 SQL 不同（`V2_SCHEMA` 与 `identity.VIEWS`+`HYBRID_*`），都原样执行；图的算法只在 hybrid 下运行，未设置时 `parent_id`、`merged_into` 是直接状态。
- `v2` 的库拒绝切换，留在旧表并提示原因（待确认，见 D1）。
- 切换后拒绝 `activate_identity`：改策略是迁移，不是日常写。

## 4. 逐表映射（修订）

「写入方」一列是切换后的写入方。

### 4.1 词条、结构、链接

| 旧表 | 主数据 | 切换后 Python 读到的 | 写入方 |
|---|---|---|---|
| `entities` | `terms`（key = 原 id，kind 原样）+ attrs + `term_tree` + 身份图状态 | 兼容表 `entities`，15 列见 4.4 | 核心 |
| `identity_baseline` | `term_identity_baseline`（活表，行序 = 原 rowid 顺序） | 冻结改名 `legacy_identity_baseline` | 核心 |
| `links` | `term_links`；角色的 `wd` → `wd_character`，其余 system、key、current、source、added_at 原样 | 同名视图，范围 = kind 为 character / skin / work 的词条；关键查询做 EXPLAIN，走不了索引就改成兼容表（同样有写保护） | 核心 |
| `work_review` | `term_tree` 的 status / note / locked 三列原值 | 兼容表 `work_review` | 核心 |
| `identity_nodes`（视图） | — | 原样，建在兼容表上 | — |

### 4.2 人工结论与关系

| 旧表 | 主数据 | 切换后 | 写入方 |
|---|---|---|---|
| `assertions` | 对 (图, 原始词条) 的 `term_verdicts`：confirmed→yes，rejected→no；`via` 保存原 origin | 旧表冻结为 `legacy_assertions`；兼容表 `assertions` = 每个目标的有效裁决，origin = `via` | 核心 |
| `identity_relations` | `term_relations`：`duplicate_of(a,b)` → `same_as(a,b)`（b 是代表）；`form_of(a=皮肤,b=人物)` → `contains(b→a)`，**端点反转**；`different_from` → `different_from` | 冻结；兼容表保留原 id（新关系 id 大于旧的最大值），`different_from` 按原来的**文字**顺序输出 a < b（`web/identity.py（#9 已删除）:244`），status 由最新裁决算出（yes = accepted，no = rejected，clear 或没有 = proposed） | 核心 |
| `identity_evidence` | 关系证据：source 原样（`migration`、`legacy-hierarchy`、`manual`……，都不在保留字里），payload 进 data，原 sha256 指纹进 `data.legacy_fingerprint`；核心指纹取它的前 16 字节 | 冻结；兼容表 | 核心 |
| `identity_decisions` | 关系裁决：有 action 的挂 action（经 `before_json` 里的 `decision_id`）；`activate` 写的挂 `migration` 批次；reason 原样；previous_status 由序列算出，并存进裁决 data 以便核对 | 冻结；兼容表保留原 id | 核心 |
| `identity_decision_evidence` | 裁决的 `reviewed`（证据 id） | 冻结；兼容表 | 核心 |
| `identity_uncertain`、`identity_anchor_decisions`、`identity_anchor_history` | 私有人工状态，表不变 | 原表，核心写 | 核心 |
| `deferred` | 外围（复核流程） | 原表，核心在同一步里写 | 核心 |

### 4.3 证据、私有状态、投影、外围

| 旧表 | 去处 | 切换后 | 写入方 |
|---|---|---|---|
| `candidates` | 证据：WD → `wd.wd_character`，CCIP → `ccip_index.char_ext`，量纲 `wd_probability` / `ccip_score`；按有效批次生效 | 兼容表：有效证据按 (uuid, 原始词条, WD/CCIP) 取最大分（= `INSERT_CANDIDATE`） | 核心 |
| `proto_claims`、`weak_claims`、`prototypes`、`learn_state` | 判定器私有，整批替换 | 原表 | 核心（由学习批次写入） |
| `identity_canonical`、`identity_scopes`、`identity_suspended`、`identity_claims`、`identity_pairs`、`identity_calibration`；meta 的 `identity_graph_version`、`identity_calibration_active`、`identity_bank`、`identity_bank_error` | 私有或投影 | 原表、原键 | 核心 |
| `memberships`、`tag_memberships`、`family_evidence`、`single_assets`、`lead_assets` | 投影 | 兼容表，由核心执行原 SQL 写；另把人物、皮肤的 confirmed / machine 镜像进 `term_projection`（这一步没有读者） | 核心 |
| `membership_rules`、`family_confirmed`、`effective_assertions`、`variant_memberships`、`identity_assignments`、`ambiguous_claims` | 规则视图 | 原样保留 | — |
| `names`、`display_names`、`name_aliases`、`name_cache`、`name_settings` | 这一步不收编 | 原表；用户名字和核心命令产生的别名由核心按 `put_name` 语义写，NameUpdater 照旧写其他来源 | 核心 + Python |
| `asked`、`reviews`、`imported`、`prefs.pins` | 外围 | 原表，由所在那一步的核心命令一起写 | 核心 |
| `audit_sample`、`prefs.identity_skipped`、`curation` | 外围 | 不变 | Python |
| `duplicates` | 外围；分组由 Python 预先算好交给核心，`hidden` 由核心折叠 | 原表 | 核心（导入时）+ Python（`curate`、相似分组） |
| `auto_rules`、`excluded` | 外围 | 原表；导入命令里由核心执行原 SQL，`curate` 仍由 Python 写，两者由写锁串行 | 核心 + Python |
| `backend_effects` | 外围 | 原表，核心同事务写 | 核心 + Python |
| `relations` | 图片相似关系，与本步无关（TERMS_SCHEMA 第 5 节误写成作品层级，之后更正） | 不变 | Python |

### 4.4 完整字段映射

**entities（15 列）**

| 列 | 兼容表的值 | 说明 |
|---|---|---|
| `id` | `terms.key` | 界面链接不变 |
| `name` | `''` | 切换前预检全部为 `''`，否则拒绝（`tests/reference/web/store.py（#9 已删除）:491` 已清空） |
| `work` | `term_tree.work_text` 原样 | 不由 qualifier 重算：未知版权后缀的角色可能有字符串而 `work_id` 为空（`tests/reference/web/store.py（#9 已删除）:1070–1079`，`tests/test_works.py:44–51`）。写入规则照 `_entity`、`_set_work`、`set_work`、`set_kind`（原创时清空）、`_undo_profile` |
| `external_key`、`tag` | `NULL` | 预检全部为空（`tests/reference/web/store.py（#9 已删除）:452–460`） |
| `variant` | hybrid：图投影（有父人物时取基线 variant，没有就取 Danbooru 链接，再没有就取 id；否则 `''`，`web/identity.py（#9 已删除）:372–377`）；未设置策略：attrs 原样 | |
| `parent_id` | 作品：`term_tree`；人物、皮肤：hybrid 用图投影，未设置策略用直接状态 | |
| `merged_into` | hybrid：图投影（代表不是自己时）；未设置策略：直接状态（破坏式合并） | |
| `kind` | 作品：`work`；hybrid 下有父为 `skin`，否则取基线 kind；未设置策略下原样 | |
| `artist`、`label_only`、`mascot`、`mascot_origin`、`original` | attrs 原样 | mascot 这一步不改成 kind（见 D6） |
| `work_id` | `term_tree`（角色）；皮肤为 `NULL` | |

- **assertions**（uuid, entity, status, origin）：有效裁决；origin 取 `via`（review / sweep / album，基线行保存原值）。
- **candidates**（uuid, entity, source, score）：见 4.3；entity 是 `_candidate` 解析出的**原始**词条（hybrid 不走合并，未设置策略时走 `_resolve`，`tests/reference/web/store.py（#9 已删除）:1100–1102`）。
- **identity_relations**（id, a, b, kind, status）、**identity_decisions**（id, relation_id, previous_status, status, reason, created_at）、**identity_decision_evidence**（decision_id, fingerprint）、**identity_evidence**（relation_id, fingerprint, source, payload, created_at）：id、指纹、时间都取原值。
- **memberships / tag_memberships**：9 列由原 SQL 产生，包括 skey 的 `printf`（`tests/reference/web/store.py（#9 已删除）:672–675`）。
- **links**（system, key, entity_id, current, source, added_at）：`wd_character` 映射回 `wd`；entity_id = 词条 key。

## 5. 既有语义（逐行移植，写进测试）

### 5.1 身份图（hybrid）

1. 每次重建前先追加基线：`entities` 里还不在 `term_identity_baseline` 的身份，按当时的 parent_id / variant / kind / merged_into 追加（= `baseline()`，`web/identity.py（#9 已删除）:233–235`）。新身份的层级（`_hierarchy` 的名字规则）只在这里，不是关系，人工不能覆盖（原行为）。
2. `_graph`：
   - 基线行按原 rowid 顺序；先按基线 `merged_into` 合并，再按 accepted 的 `duplicate_of`（**按关系 id 顺序**）合并，`join(a,b)` 的根取 b 那一侧；
   - 再在等价组上挂层级：先挂基线父，后挂 accepted 的 `form_of`（按 id）；
   - 出错条件：变体与其人物重复、变体已有另一个父人物、层级成环、`different_from` 与等价组或层级冲突；
   - 不限制同层合并（审核反例：独立人物的别名可以并进已有的皮肤组）。
3. `_project`：
   - 先按 scopes 判断等价组是否真拆开，拆开的挂起；认领被拆开的改成待定；
   - 重写 canonical，逐行写回 entities；
   - `graph_version` = `json.dumps([rows, sorted(pending)])` 的 sha256，按 Python 默认（`ensure_ascii`、`', '` 分隔）逐字节复刻（冻结分支 `identity_graph.rs::legacy_json` 与其测试可作对照）；
   - 版本变了：失效校准、清认领；hybrid 下对受影响的图（`affected_images` 加挂起的图）重算证据与归属。
4. `accept_many` 逐条接受、逐条校验，最后重建一次。`decide` 每次都追加一条决议，并记下当时关系上全部证据的指纹，包括退回 proposed（`web/identity.py（#9 已删除）:412–424`）。
5. 「未复核继承层级」（`pending_hierarchy_people`）：关系上有 `legacy-hierarchy` 证据，且**最近一条裁决（含 clear）**的 reviewed 没有包含它。
6. `assertions` 的触发器语义（`web/identity.py（#9 已删除）:206–218`）：写一条结论时，删掉该 (图, 身份) 的 scope / suspended；所在等价组多于一个成员时，按组成员重写 scope。移进核心写裁决的同一段代码，别处不再触发。

### 5.2 作品树（状态，不是重算）

- `create`：新作品一律上锁（有无上级都锁，`tests/reference/web/works.py（#9 已删除）:22–30`）。`move`：先查不成环（子树包含目标就拒绝），然后上锁。`link`：旧的 Danbooru 链接改为非当前、新链接为当前，status 改 confirmed、note 清空。清空链接：status = confirmed，note = 「外部链接已由用户清空」。
- `apply_evidence` 逐行移植（冻结分支 `works.rs:157–337` 对照）：
  - 遍历 qualifier 的顺序 = 原查询的行序；
  - 按 tag 排序处理上级候选；
  - 已上锁的跳过；多个候选只写 note、**保留已有上级**；成环只写 note；只有一个时直接覆盖未上锁的上级。
- 两个反例进夹具：已挂 B 的 A 收到 B、C 两个上级 → 仍是 A→B；A、B 互为上级 → 只接受先处理的 A→B。
- 在应用里它只被 `tools/reconcile_works.py`（2026-10-05 删除，#186）调用；核心提供同名命令，工具改为调用核心（第 8 节）。
- 子树计数照旧用 `works.py` 的 `TREE` 查兼容表；不给作品建逐图投影。

### 5.3 归属规则

`family_evidence` 的插入语句、`membership_rules`、`variant_memberships`、`ambiguous_claims` 按当前策略的**原 SQL** 执行（`tests/reference/web/store.py（#9 已删除）:581–604`，`web/identity.py（#9 已删除）:82–155`，`tests/reference/web/store.py（#9 已删除）:195–251`）。由此自动保持审核 I3 列出的每一项：先排名后筛否定、各项条件、hybrid 皮肤视图读代表的原始结论、相册确认不能单独确认皮肤、skey 格式。

## 6. 历史重建与撤销

### 6.1 三种等价

- **当前状态等价**：收编同步之后，所有兼容表与旧表逐行逐列相等。
- **连续撤销等价**：从同一状态出发，新旧两边反复「撤销最近一步」，每撤一步后所有兼容表仍相等，一直撤到旧历史的第一步。
- **增量与全量一致**：任何核心命令之后，从 `term_*` 全量重建出的兼容表，与增量维护的结果相等。

### 6.2 按目标重建裁决（assertions）

对每个 (图, 身份)，把**未撤销**的步骤按 id 排成事件序列：

| 事件 | 设成的值 |
|---|---|
| 快照项（有 entity） | 下一事件的 before；没有下一事件就是当前值。它一定写了一行 review / sweep（`change`、`answer`、`answer_audit`、`reassign`、`sweep` 都用 `INSERT OR REPLACE`），所以设成的值必须是这种行，否则记为异常 |
| 破坏式合并（`merge`，只在策略未设置时产生） | 源：删除（在 M 上记 clear）；目标：moved 的值（在 M 上记 yes / no），kept 的目标保留自己的 |

基线（非用户批次）：

- **初值**：第一事件的 before 是一行时，建批次，anchor = 第一事件 id − 1；album 行进 `album` 批次，其他进 `baseline` 批次，`via` 保存原 origin。
- **中途出现的值**：某事件的 before 与前一事件设成的值不同，只可能是「前一步已撤销、又被相册导入写入」这类无 action 的写入，建批次，anchor = 该事件 id − 1。
- **没有任何事件的当前行**：anchor = 0。
- **无法解释**：设成的值不是 review / sweep 行，或当前值与最后一个已知值矛盾，就计数、报出样例，**不归给最近的 action**。真实副本上这个计数必须为 0，或者逐条列给用户决定之后才能切换。

为什么能连续撤销：Python 每次只撤最近一步，恢复的是该步的 before；核心把那一步标为撤销后，有效裁决回到按锚点排序的上一条，而重建保证这一条就是 before。相册导入的 `INSERT OR IGNORE` 在有结论时不写，切换后 album 批次同样只在「没有有效结论」时写入，不改成覆盖。

身份关系按同样的办法处理：有 action 的决议挂 action；`activate` 写的决议挂 `migration` 批次（它们没有 reviewed，所以继承层级仍是「未复核」）；旧 `before_json` 里的 `relation_id`、`decision_id` 经 `term_legacy_ids` 对应。

`deferred`、`identity_*` 私有状态、`asked`、`reviews` 不需要重建：旧步骤撤销时直接用原 `before_json` 恢复（与 Python 相同），它们是原表。

### 6.3 撤销适配器（切换后）

归属规则（Python `terms_core.owns` 与 Rust `owns` 同步改）：切换后，`before_json` 的每一项都是下列形状之一的步骤归核心，其他的（`[]`、`curation`、`identity_skip`）仍归 Python；形状未知，或一步里既有核心形状又有 Python 形状，核心拒绝并提示。

| 形状 | 核心的撤销 |
|---|---|
| 快照 `{uuid, entity, assertion, deferred[, identity]}`（entity 可空） | 标记撤销，有效裁决随之回退；`deferred` 按原值恢复；有 `identity` 就恢复 scope / suspended / uncertain（`restore_scope`）；重算这些图的归属与折叠；删 `asked` 中该 action 的记录 |
| `merge`，策略未设置 | 标记撤销：M 上的 clear / yes / no 失效，`merged_into`、候选、皮肤父、别名、pins、reviews 按快照恢复（= `_undo_merge`，`tests/reference/web/store.py（#9 已删除）:2453–2471`） |
| `merge`，hybrid（激活前留下的旧合并） | 在 `undo_audit` 批次追加对应 `duplicate_of` 的 clear（reason「撤销历史合并；原归属无法区分的认领待确认」，reviewed = 当时的全部证据），再重建图；找不到迁移记录时照原样拒绝 |
| `identity_relation` | 标记撤销，并在 `undo_audit` 批次追加 previous_status 对应的裁决（reason「撤销人工关系决议」，reviewed = 当时的全部证据），与旧版的 `decide(previous)` 等价，因此「未复核」的判定一致；再重建图 |
| `identity_anchor` | 恢复原锚点结论、追加历史 `undo`、失效校准、清认领、重算归属；不恢复旧的机器认领 |
| `profile` | 恢复用户名字（`names`）、Danbooru 链接、`work`/`work_id` 及其皮肤的 `work`；链接变了就重建图 |
| `artist_verdict`、`{uuid, tag, had}`、`terms` | 与第 2、3 步相同 |

- 两边都要保留的：删掉该 action 的 `reviews`；最近一步核验（核心在自己的事务里再核对一次）。
- 挂起只在等价组**真的拆开**时触发（`tests/test_identity.py:51–87`）。
- **新步骤**写成 `[{"terms": {"v": 2, "type": …, …}}]`；零成员的 `sweep` 也是非空数组，仍写 `reviews`。
- `names.action_label` 按快照里的 entity id 重新渲染历史文案（`tests/reference/web/names.py（#9 已删除）:487–514`），核心步骤里要保留同样的 entity id 字段，并改 `action_label` 读取它，否则改名后历史文案会与旧版不同。这一项计入等价验收。

## 7. 证据批次

- **可替换来源**：`wd.wd_character`（source.sqlite `labels` dim=character、conf ≥ 0.35）和 `ccip_index.char_ext`（dim=char_ext）。每个批次写明覆盖范围：
  - `refresh` 覆盖全库；
  - `reindex_characters` 覆盖被改动的图（含索引 drop，`tests/reference/web/index_update.py（#9 已删除）:219–254`）；
  - `import_changes` 覆盖新图。
- **生效规则**：覆盖范围内每张图的当前有效证据 = 本批次的行；覆盖到但没有行（成功的空结果）= 该图这个来源的证据清空。失败的批次整事务回滚，旧的有效集不动。
- **版本与冲突**：版本 = `labels.model_ver` 加内容摘要（沿用画师的做法 `v4/<摘要>`）。参考索引换了而模型版本没变时，内容不同就另起一行，不再冲突。
- **解析**：照 `_candidate` 逐行（index `wd_tag` 唯一时用它，否则依次找 `wd` 链接、`danbooru` 链接，再没有就新建带 `wd` 链接的身份；CCIP 找 `ccip` 链接，没有就新建）。解析和建词条都在同一个核心事务里，新身份的 UUID 由核心生成，写入兼容 `entities`。同一 (图, 原始词条, 来源) 的多条标签先聚合（取最大分，标签值记进 data），再入证据表，避免撞唯一索引。
- **索引数据**：`_characters`、`_hierarchy`、`_tags`、`character_aliases`、`_default_tags` 合成一个「索引同步」命令，输入是 Python 从 source.sqlite 只读导出的 `characters` / `character_aliases` 行。`wd_tag` 不唯一，所以不当链接存，只在解析时使用（与旧行为相同）。
- **初次收编**：把现有 `candidates` 整表作为 `migration` 批次（全库覆盖）导入，保证当前状态等价；第一次真正的 `refresh` 之后，才由源数据重算。
- **学习批次**：Python 照旧算原型；发布时一次交核心：`prototypes`、`learn_state`、`proto_claims`、`weak_claims` 整批，以及变化的图。核心整批替换，并对变化的图重算归属与折叠。拒绝过期结果属于新行为，见 D6。

## 8. 写入口与事务归属

「附带」指核心在同一事务里一起写的外围或私有表；effects 是写进 `backend_effects` 的 kind。

| 入口 | 现在进 actions | 切换后 | 附带 | effects |
|---|---|---|---|---|
| `change` | 是 | 核心 `verdict` | deferred、identity_uncertain、fold | verdict |
| `answer`、`answer_audit` | 是 | 核心 `answer` | deferred、identity_uncertain、asked、fold | verdict |
| `reassign`、`correct` | 是 | 核心 `reassign`（核心内复查「原角色归属已变化」） | deferred、identity_uncertain、fold | verdict |
| `sweep` | 是 | 核心 `sweep` | reviews、fold | verdict |
| `merge` | 是 | hybrid：同 `decide_identity`；策略未设置：核心 `merge`（破坏式） | names 别名、prefs.pins、reviews、fold | verdict |
| `decide_identity`、`uncertain_identity` | 是 | 核心 | 私有表、meta、fold | verdict |
| `correct_identity_anchor` | 是 | 核心 | 锚点表、learn_state、meta | 无（与现在相同） |
| `set_profile`、`link_local` | 是 | 核心 `profile`（`copyright_of` 仍由 Python 在核心事务外联网取得） | names、work_review、图重建 | verdict |
| `skip_identity_pair`、`set_pins`、`curate` | 部分 | 不变（Python） | — | — |
| `create_entity`、`create_linked`、`shared_oc` | 否 | 核心 `create`，不进历史 | names；hybrid 下重建图 | 无 |
| `set_kind`、`set_mascot`、`set_work`、`edit_work`（含 from_tag） | 否 | 核心结构命令，不进历史 | names、work_review、受影响图的归属 | 无 |
| `import_changes`（同步） | 否 | 核心 `import-changes` 一个事务（见第 9 节） | assets、album 批次、imported、album 别名、duplicates、fold、auto_rules、excluded、sync_added、meta.indexed | imported |
| `refresh`、`reindex_characters`（含 IndexUpdater 的 add / drop） | 否 | 核心 `refresh-characters` / `reindex`，附索引同步 | 同上 | 无（与现在相同） |
| `_refresh_general_hits`（WD 重交后重算派生） | 否 | 在核心里重算 single / lead / 归属 / 规则 / 排除 | 同上 | 无 |
| 启动：`_migrate`、`_default_tags`、`_album_titles(add_aliases=True)` | 否 | Python 的迁移跳过核心的表；`_default_tags` 改由核心执行；album 别名写 `names`（Python 可写） | — | — |
| `activate_identity` | 否 | 切换后拒绝 | — | — |
| `learn._claim`（Learner） | 否 | 核心学习批次 | 私有表、fold | 无 |
| NameUpdater（`replace`、`aliases`、`seed`、`put`） | 否 | 不变：只写非用户来源的名字 | — | — |
| `undo` | — | 按第 6.3 节分派 | — | verdict |
| 工具：`reconcile_works --apply`（2026-10-05 删除，#186） | 否 | 调用核心的作品证据命令 | — | — |
| 工具：`apply_classification_repair`、`preview_classification_repair`、`identity_rollout`、`identity_bank`、`identity_metadata`、`eval_bench`（这六个已于 2026-10-05 删除，#186）；旧 Python 索引工具已在 #53 删除 | 否 | 在**任何**副作用（初始化、备份、写 source）之前检查切换标记，已切换就拒绝；纯读取的工具照常 | — | — |

新增行为「作品和类型编辑可撤销」不在本步（见第 11 节），所以这些接口的 HTTP 响应形状不变。

## 9. 跨连接协议

1. **进程级写锁**：把 `Store._labels_lock` 扩成一把 `_terms_lock`（可重入）。每个会让核心写的 Store 方法，从解析选择集到核心返回全程持有；Syncer、IndexUpdater、Learner 的发布、撤销、切换同样持有。NameUpdater 只写 Python 自己的名字表，不需要。
2. **一个业务命令 = 一个核心事务**：Python 在持锁状态下用只读事务解析 id 和文案，关掉连接后调用核心（沿用 `tests/reference/web/terms_core.py（#9 已删除）` 的约定）。核心在自己的 `BEGIN IMMEDIATE` 里重新核验：图存在、重复组展开（在核心内按 `duplicates` 重算，不信任调用方）、目标身份解析（`_resolve` / `_require_entity`）、原归属、父角色是否被移出、最近一步。
3. **选择集**：写锁保证解析和写入之间没有进程内的其他写入，结果与旧版「同一事务里解析」一致。其他进程写入由第 8 节的工具检查挡住，桌面 App 只有一个实例。
4. **后处理**：核心同事务写 `backend_effects(kind)`；Python 的 `effects.operation` 回来后照旧 `drain`（`tests/reference/web/effects.py（#9 已删除）`），学习与相似扫描照常被唤醒。
5. **折叠**：成员有变化的同一事务里，核心对受影响的组执行 `_refresh_fold` 的原 SQL（`tests/reference/web/store.py（#9 已删除）:692–695`）。
6. **导入**（B6 的主要场景）：
   - Python 在锁内只读 source.sqlite，准备好：新图行、删除列表、角色标签行、相册决定与映射、`duplicates` 全量分组（它不依赖成员）、`realistic` 列表。
   - 然后调用一次核心 `import-changes`，一个事务按旧顺序提交：assets → 候选批次 → 相册批次 + imported + 别名 → 证据与单人、线索 → 归属 → duplicates 替换 → 折叠 → auto_rules → excluded → sync_added → meta.indexed → effects。
   - 新图的 WD 仍在此之前先交，已由视图的 join assets 屏蔽（第 3 步已验证）；它挪到锁内执行。
   - 核心失败就什么都没提交，Python 不再做任何后续写入。
7. **切换屏障**：启动后在后台线程里执行，与第 3 步相同。切换全程持有写锁，期间 App 的写操作等待，读操作读旧表；切换是一个事务，失败留在旧表，下次启动再试。切换前先自动备份（见 D3）。
8. **恢复**：核心写过而 Python 还没完成的中间态已经不存在：所有写入都在核心事务里。`backend_effects` 仍是可重放的待办记录。

## 10. 名字：本步边界与下一步契约

**本步**：`names`、`display_names`、`name_aliases`、`name_cache`、`name_settings` 仍是 Python 的表，`View`、`entity_name()`、`entity_info()`、`work_name()`、`search_hits`、`action_label` 都不改。核心命令写名字时逐行复刻 `put_name`：

- display 只接受有语言、来源在优先级表里的名字；来源为 danbooru 的 display 丢弃；
- 设首选时，先把同语言其他名字的首选清掉；
- upsert 时首选取较大值。

NameUpdater 不变，名字版本触发器仍在这几张实体表上，照常触发。

**名字收编那一步的前置契约**（B5，现在不做）：

- **显示投影要有的字段**：完整显示名、皮肤短名、来源、三种语言、tag。
- **输入要覆盖**：带 qualifier 的作品读 `display_names`、`WORK_NAMES` / `SKINS` 例外、英文回退到可读 tag、合并的代表借用成员的名字、原创角色和本地角色只用自己起的名字。
- **名字包要能表达**：display / alias、position、preferred，以及「按来源整批替换」和成功的空结果；现在的包只有 lang、value、source，而且会删掉同来源的其他名字（`crates/core/src/package.rs:493–516`，`store.rs:594–600`）。
- **失效依赖**：实体属性、链接、canonical、父人物、词表。
- **读路径**：`_display_reader` 和 `action_label` 直接用 `View`，也要切过去。
- **对照**：与 `View.entity`、`choose`、`search_hits` 全量对照。

## 11. 用户可见的行为变化（本步都不做）

| 变化 | 原因或来源 | 状态 |
|---|---|---|
| 作品新建 / 改名 / 移动 / 链接、角色归作品、设为原创、当作标签变成可撤销的步骤 | TERMS.md「裁决带 action_id」 | 延后；要改撤销栈，并定下前端怎么显示 |
| mascot 改成 `kind=label` | TERMS.md「只当标签只是改 kind」 | 延后；现在的 `set_kind` 会写一步 action，与「不进历史」矛盾 |
| 作品树改成「由证据重算」、不依赖处理顺序 | 词条模型 | 延后；两个反例的结果会变 |
| `same_as` 只允许同层 | 我原方案 | 撤回；列为之后的规则变更 |
| 身份基线边可以被人工覆盖 | 词条模型「裁决先于证据」 | 延后 |
| 启用 v2 保守认领 | 校准门槛未达 | 延后 |
| 学习批次发布时拒绝过期结果 | 审核问题 7 | 内部行为，界面看不到；见 D6 |
| 相册导入进撤销栈 | 我原方案 | 撤回；保持不进 |

## 12. 分段计划与验收

| 段 | 内容 | 验收 |
|---|---|---|
| S0 契约与夹具 | 本文确认；在**旧代码**上写夹具测试钉住现有行为：作品的两个反例、跨层合并、qualifier 作品读词表名、相册导入后人工否定再撤销、撤销后再相册导入、旧合并在 hybrid 下撤销、`identity_relation` 撤销后的 reviewed、零成员 `sweep`、混合与未知形状的步骤、空的成功批次、索引 drop | 全部在旧 Python 上通过；这批测试以后原样用于新代码 |
| S1 schema v2 | 第 2 节 | v1→v2 单事务、失败不留半套；画师和标签测试全部通过、结果不变；写保护触发器生效；Rust clippy 零警告 |
| S2 影子收编 `sync-characters` | 只读旧表，写 `term_*`，在影子前缀（例如 `shadow_entities`）下生成兼容表 | **当前状态等价**：影子兼容表与旧表逐行逐列相等（第 4.4 节全部列）；再跑一次不新增行；故意破坏一处能报出；预检失败或出处无法解释时整次回滚；只接受未设置策略与 hybrid |
| S3 核心命令与撤销（双跑） | 第 5、6.3、7 节的全部命令；测试框架对同一个夹具库的两份拷贝，一份走旧 Python、一份走核心，执行同一串操作 | 每步之后兼容表相等；**连续撤销等价**：撤到最早的一步，每一步都相等；**增量与全量一致**：每条命令后全量重建等于增量结果；跨连接失败（核心中途报错）什么都没留下 |
| S4 切换与接线 | 切换命令、Python 分支、写锁、effects、折叠、owns、工具检查、`_migrate` 跳过核心的表 | 三层测试（旧态夹具、迁移、切换后命令）全部通过；Python 写兼容表被拒绝；工具在副作用之前拒绝；重启后恢复；切换失败留在旧表 |
| S5 真实副本（**逐次授权**） | 一致性副本上重复 S2–S4 | 同步 0 差异；出处无法解释为 0 或已逐条确认；对真实历史做连续撤销对照（在副本上撤 N 步，N 与你商定）；抽样操作双跑；切换后旧读路径、核心、App 读路径三方一致；故意破坏一处能报出；性能按第 13 节测量 |
| S6 文档 | 写 `docs/TERMS_CHARACTERS.md`；更正 TERMS_SCHEMA 第 5 节与交接文档的误写 | — |

重点回归：`test_identity`、`test_identity_hybrid`、`test_web_v2`、`test_lead`、`test_works`、`test_names`、`test_character_profile`、`test_ask`、`test_sync`、`test_backend_tasks`、`test_index_update`、`test_learn`，以及 `test_terms_*`。

## 13. 性能：只测量、记录

测量项：

- 同步、切换的耗时；
- 每类命令的事务时长与写入行数（写放大：兼容表 + 词条表 + 私有表）；
- 一次关系变更影响的图数；
- WAL 与库体积变化；
- `links` 视图关键查询的 EXPLAIN QUERY PLAN 与计时；
- 撤销的时长；
- 证据批次集合表的增长。

与旧版同一批查询对比（沿用 TERMS_LABELS 第 4 节的表格形式）。不根据「用了 Rust」推断性能提升。

## 14. 最小待确认清单

**2026-09-28 用户确认：D1–D6 全部按下表的建议执行。** 按 S0 → S6 顺序开工。

原九个问题和审核新增的四项，合并成下面六项。审核已同意、没有分歧的不再列出：副本到 S5 再申请；热派生表物化；学习产物作私有状态；离线工具要在副作用之前拒绝。

| # | 决定 | 我的建议 | 理由 |
|---|---|---|---|
| D1 | 支持哪些身份策略 | 支持未设置和 hybrid；v2 的库拒绝切换，留在旧表并提示 | 新图库默认未设置、生产据称是 hybrid；两者的规则都是现成的 SQL，可以原样执行。v2 的校准门槛未达，没有真实使用 |
| D2 | 撤销边界与基线 | 用非用户批次加排序锚点；旧历史的每一步都能按原样连续撤销；基线不进撤销栈、不能撤；出处无法解释的计数为 0 才能切换，否则逐条给你看；相册导入切换后仍不进撤销栈 | 不伪造历史，也不挡住撤销栈；NULL action 方案要改所有读取，且表达不了 album 语义（审核意见 5） |
| D3 | 切换方式与回退 | 启动后后台切换，全程持进程写锁，期间写操作等待；回退 = 恢复切换前的自动备份，**切换之后做的操作会丢失**；旧人工表冻结保留，供核对 | 与第 3 步相同；做不到无损回退，所以明确说出来 |
| D4 | 名字是否本步收编 | 不收编：表和规则留 Python，核心命令同事务写用户名字和别名；名字投影作下一步，第 10 节的契约是它的前置条件 | 角色判定和作品树都不依赖名字；B5 的契约很大，拆出去能保住本步的等价边界 |
| D5 | 派生表形态 | 兼容表由核心写、加写保护；`links` 用视图（EXPLAIN 走不了索引就改成表）；规则视图原样保留、由核心执行同一段 SQL | 等价由构造保证；热表避开视图的性能风险；漏改的 Python 写入会直接报错，不会悄悄成功 |
| D6 | 行为变化 | 第 11 节全部延后，本步只做等价迁移；唯一建议纳入的是「学习批次拒绝过期结果」（界面看不到，旧版本来就没有保证） | 符合「不混入迁移」；过期学习结果本来就会在下次学习时纠正，拒绝只是提前重来 |

## 15. 仍未解决、需要副本核实的

- 生产的 `identity_policy`；激活前留下的 `merge` 步骤有多少；`identity_baseline` 的扫描顺序是否就是 rowid 顺序（移植时显式按 rowid 排序，并在副本上对照）。
- 真实历史里「出处无法解释」的数量，以及相册行在历史中间出现的次数。
- `candidates` 与 source.sqlite `labels` 的对应：重扫以后，同一版本下内容不同的情况有多少；`model_ver` 与 `run_id` 的实际取值。
- `links` 视图的查询计划；兼容表的写放大与库体积。
- 名字：本步不动，不需要核实；下一步再做。

## 16. 实施进度

S0–S6 已全部完成（2026-09-29）。2026-09-30 精简：本节只保留代码和工具仍在引用的部分——S3（命令一览、已批准保留的旧行为与验收例外）、main 整合的对照方法、S5 总结（允许的例外清单）。S0–S2、S4、S5 各部分的逐步流水、S5 复测、合入 main 与 S6 的记录已删除，原文见开源前私有历史；结果见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 第 6 节。

### S3 学习发布、原规则 SQL 与比较器（2026-09-29，部分实现；S3 未完成）

**范围与状态**：已实现 `learning-snapshot`、`publish-learning` 和派生表重算 `reproject-characters`。核心在同一写事务替换 `prototypes` / `learn_state` / `proto_claims` / `weak_claims`，沿用 `_claim` 的变化图算法，再执行旧 SQL 更新归属、折叠及人物/皮肤的 `term_projection` 镜像。Python 只计算原型与分数、组装完整批次；`LearningPublisher` 是 S3 双跑的显式接缝，应用默认仍走旧写入方，自动接线留 S4。没有新增依赖，没有修改 S0 的 `switch()` 或断言，没有访问真实图库。

`character_projection.rs` 执行 `character_sql/` 中逐字保存的旧 SQL；测试重新捕获 Python `_refresh_*` 执行的 SQL 并核对原文。局部刷新保留 ≤10 张图逐图执行、更多图走 `touched` 的原分支。学习可以首次认领 S2 尚未登记到 `term_assets` 的图：镜像时沿用 `store::library_asset` 登记，不能因内连接而漏掉这些归属。

**D6：过期判断与处理**（第 11 节唯一纳入的行为变化，用户再次确认）：

- `learning-snapshot` 在只读事务里，按固定顺序对学习依赖生成带版本的 SHA-256 指纹：资产、实体、链接、人工结论和完整 actions（含 undone）、候选、单人/线索、重复组和图片关系、身份基线/关系/决议/人工私有状态、当前学习四表，以及身份策略/图版本/校准/索引相关 meta。按列类型与长度编码，BLOB 原字节参与；不依赖行数、最大 action id 或可复用的 rowid。
- Python 随后在同一只读快照里准备训练数据、完整原型和认领批次，关连接，再交核心。发布的 `BEGIN IMMEDIATE` 内先重新算指纹；不相等返回 `status=stale`，没有任何写入，也不更新 `learn_state`。两次快照之间发生变化会保守拒绝。人工结论先改再撤回仍有历史变化，拒绝；另一个 Learner 已发布过也会拒绝旧批次。
- Learner 收到 stale：保留 `regroup` 请求、不聚类、不显示错误，重新设置 wake，按原有 2 秒间隔和静默窗口再读新快照重来；对外状态仍为 idle。成功响应的内部 `published` 状态也不会覆盖原有 idle 状态。此路径通过显式 publisher 测试，生产自动启用仍属于 S4。
- 指纹覆盖本库依赖；训练使用该轮 Features 已加载的内存特征和 anchors。S4 的发布写锁、索引重载接线仍须完成，不能据此宣称所有外部索引并发场景已验收。

**旧分数阈值与已批准的验收例外**：旧 `_claim` 只有新增/消失的认领，或同一 `(uuid, entity)` 相比前一批的分数变化 **> 1e-6**，才刷新该图的派生表。≤1e-6 时新分数仍写入 `proto_claims`，原派生值保留。Rust 原样保留该判断，取消阈值留给以后的规则变更。

复现（临时合成图 `close`）：认领分数从 `0.9956405460834503` 到 `0.9956410460834503`，变化图数为 0；增量归属保持前者，全量重算为后者。已实际调用 SQLite 核实 `%020.12f`：分别输出 `0000000.995640546083` 和 `0000000.995641046083`。宽度 20 是最小宽度，小数位为 12；这次 5e-7 的变化确实反映在 skey 中。

`tests/terms_projection_compare.py::assert_rebuilt_equal` 只在以下条件同时满足时接受差异：

1. 比较的是 **publish-learning 之后的增量与全量归属**，且该图/角色明确属于被阈值略过的分数影响范围。仅 `memberships`、`tag_memberships`、人物/皮肤的 `term_projection`；其他命令和其他表仍严格相等。
2. 同图、同角色的行集合完全相等，两边分数有限且非 NULL，差的绝对值 ≤1e-6。status/state、origin、sources、source_count、ccip_only 和其余字段全部相等。
3. skey 只能随本行 score 变化：逐边调用 SQLite 原 `printf` 重新验证完整字符串，首部各标志和最后的 family-confirmed 标志不得变化。不能跳过整个 skey 或截断比较。
4. **旧 Python 与核心的逐步双跑不调用容差比较器**，所有列精确相等。夹具包括学习发布、人工操作、逐步撤回至最早旧步骤后的再学习；这里的撤销仍是旧 Python 撤销，不能算作核心撤销已完成。

**范围外必须报差的实际反例（未扩大例外，完整 S3 全量一致验收仍未通过）**：

- 上述 `close` 复现中，`family_evidence.ccip` 也随全量重建变化；它不属于用户批准的 score / skey 范围，比较器严格报差。
- 单人图，两个人物的 PROTO 分数 Alice=`0.9499998`、Bob=`0.95`。只把 Alice 改为 `0.9500003`（差约 5e-7），旧 Python 和核心都报告变化图数 0，增量归属仍是 Bob；全量重建的归属变为 Alice。**行集合变化**，必须拒绝，不能当作小分数容差通过。两种身份策略的夹具均复现此行为，且旧新双跑完全相等。用户已批准保留这些旧行为，记录全量一致验收失败并继续实现；实现没有改规则。

**测试**：`tests/test_terms_character_learning.py` 当前 29 项，包括两种策略的精确双跑、成功空批次、学习期间人工结论变化后拒绝并在下一次重试成功、改后撤回、两个工作者、输入错误和四表替换后的中途 SQL 故障整次回滚、≤10/>10 图增量路径、首次认领的资产登记与镜像、Learner 重试及对外状态。比较器反例覆盖超 1e-6、换图/换角色、增删/重复行、status/origin/sources 等变化、错误 skey、NULL/NaN、非学习命令，以及上面真实的排序翻转。断言“检测到差异”的测试通过，**不代表对应全量一致验收通过**。

Rust 新增 3 项（批次形状/BLOB/重复目标、类型敏感且稳定的指纹/撤销历史/回滚、stale 不留 writer 键）；`cargo test` 67 + 3 项通过，clippy 零警告，fmt 已执行。Python 全量最终结果见本段末尾。

**性能（只测量）**：release 构建，同一合成夹具（18 图、2 身份、6 张人工种子，产生 1 原型、8 条认领），5 次中位数。未设置策略 / hybrid：快照 CLI 为 6.961 / 7.785 ms，发布 CLI 为 10.385 / 13.973 ms；旧 Python 完整训练加发布为 5.159 / 7.576 ms。CLI 数字含进程启动，发布数字不含 Python 训练；这些口径不能直接用于判断 Rust 加速，且本次全量回归同时在运行。尚未单独测纯事务时长或真实规模。

另一次只用于数行的临时库用 AFTER 触发器记录写入（不计日志表自身）：family_evidence 插 8；memberships 删 6 插 8；learn_state 插 1；proto_claims 插 8；prototypes 插 1；term_assets 插 2；term_projection 删 6 插 8；meta 插/删各 1，共 50 次行操作。带测量触发器的 WAL 增加 78,280 字节，数据库页占用保持 897,024 字节；测量对象包含审计开销，不当作生产体积估计。没有做性能优化；唯一修正是本段新增完整批次组装时，将逐行查 dirty 列表改为集合，避免新代码的平方级查找。

**尚未完成**：身份图/关系命令、作品与结构命令、裁决及全部核心撤销适配器、索引/候选/导入命令等 S3 其余内容；从 term_* 重新生成全部兼容表后再跑规则的完整重建入口；所有命令的逐步精确双跑与核心连续撤销。`reproject-characters` 当前只从兼容表重算派生表，不能冒充完整 term_* 重建。S3 不标完成，不进行 S4/S5，不合并 main。

**本次全量检查结果**：Python `unittest discover -s tests -q` 共 474 项，473 通过、1 跳过（缺研究数据，同 S0–S2）；29 项新增学习/比较器夹具全部通过。Rust 67 + 3 项通过，`cargo clippy --all-targets -- -D warnings` 零警告，`cargo fmt` 已执行并通过 `--check`。这些是当前部分实现的回归结果，不是 S3 完成结论。

### S3 身份图与关系命令（2026-09-29，部分实现；S3 未完成）

当前工作区已增加核心身份图、关系提议/决议、逐条校验的 `accept-many`、关系撤销和 hybrid 旧破坏式合并撤销；尚未完成本段全量检查与提交。旧历史合并搬动结论仍保留在 migration 批次，不随撤销搬回。新关系动作使用 v2 历史信封；撤销前校验整组历史形状，未知混合历史整次拒绝。`verify-characters` 从 term_* 重建影子兼容表进行核对，不通过重新收编旧表掩盖差异；它尚不能替代最终完整重建入口。

**已批准保留的旧行为，仍记录全量一致验收失败**：合成 hybrid 库里，图片 a 有 Alice 的相册确认，另有指向 Bob/独立人物等价组的 assigned identity_claims；合并该组再撤销时，旧 `_project` 把认领改为 undetermined / equivalence_split，并写 identity_uncertain。其刷新图片集合只来自变动身份的 candidates、proto_claims、assertions，以及 identity_scopes 的拆组图片；identity_claims 拆组新增的未定图片未补入集合。图片 a 因而仍保留原 confirmed/album 归属，全量执行旧 SQL 时该归属却被 identity_uncertain 排除。夹具同时覆盖平面 scope 与按 character/variant 分组的 scope。

复现测试：`tests/test_terms_character_identity.py::IdentityCommands.test_split_scopes_and_flat_or_object_claims_are_preserved_exactly`。旧 Python 与核心的兼容表、派生表、私有状态及规范化历史先通过精确比较，随后全量比较失败：增量 memberships 有图片 a 的 Alice 行，全量 memberships 为空。这一差异发生在关系撤销后，且改变行集合，不属于学习发布的 score/skey 容差。用户已批准保留该旧行为并继续实现。测试保持旧新双跑逐列精确相等，另明确断言全量比较必须报差，并钉住唯一差异为图片 a 的 confirmed/album 行；其余投影表精确相等。未修改旧算法、未扩大比较器例外。这个“能检测差异”的测试通过不代表全量一致验收通过。

**本段验收（更新）**：新增 11 项身份命令夹具通过，包括跨层 duplicate 的 b 代表、变体名回退、连续关系撤销、proposed/clear 与证据审阅、Unicode/浮点指纹、工具逐条接受与矛盾回退、整次故障回滚、hybrid 历史合并撤销、校准失效边界、未知混合快照拒绝、拆组反例和冗余边撤销。旧新每步兼容表/私有状态/历史精确比对；已批准的拆组反例单独断言全量比较报差。Rust 68 + 3 项通过，clippy 零警告，fmt 与 fmt --check 通过；Python 全量 485 项，484 通过、1 跳过（缺研究数据）。S0 夹具与断言未改。本段检查已完成，替代本小节开头的待检查状态；S3 整体仍未完成。

**性能（只测量）**：临时小库，同一次等价关系接受并撤销后重试，5 次中位数：旧 Python 决议 4.041 ms，核心 CLI 14.271 ms。本次为 debug 二进制，CLI 包括进程启动，且全量回归同时运行；仅记录该口径，不作生产性能或加速结论。修正了一处本段新代码的复杂度错误：原先每接受一个关系都重写全部四张关系兼容表，会增加平方级行写入；现只重建该关系的四表行。原算法逐边图校验保持不变，没有优化旧规则。

**仍未完成**：普通裁决/快照撤销、身份未定/锚点修正、作品/结构/资料命令、索引/导入和完整兼容表重建等；新身份命令通过显式 CLI 双跑，尚未接入应用的自动切换与统一撤销路由。不得以本段测试通过宣称 S3 全部完成。

### S3 其余命令、统一撤销与验收（2026-09-29，完成；S3 全部完成）

接着上面两段（学习发布、身份关系，Codex 完成）做完了 S3 的其余部分。所有角色业务命令都经 `illustash-terms character-command --db <库>`，标准输入给一个 JSON 命令 `{"op": …}`，一个核心事务；`effect` 给出时同事务写 `backend_effects`（与旧版相同：结构命令、锚点修正、refresh / reindex 不写）。

| 模块 | 命令（op） |
|---|---|
| `character_commands.rs` | `change`、`answer`、`answer_audit`、`reassign`（`correct` 同它）、`sweep`、`uncertain`、`anchor`、统一撤销 `undo`；身份关系的 `propose`、`decide`、`accept-many`、`rebuild` 转给 `character_identity.rs` |
| `character_merge.rs` | `merge`：hybrid 下是一条 `duplicate_of` 决议；策略未设置时是破坏式合并及撤销（= `_undo_merge`） |
| `character_profile.rs` | `set_profile`、`link_local` 及 `profile` 撤销 |
| `character_structure.rs` | `create_entity`、`create_linked`、`shared_oc`、`set_kind`、`set_mascot`、`set_work`、`edit_work`（create / edit / from_tag / move / link / rename）、`work_evidence`（= `works.apply_evidence`，tools/reconcile_works.py 的入口） |
| `character_import.rs` | `refresh`、`reindex`、`import`、`rederive`（= `_refresh_general_hits`）、`default_tags`（启动时的 `_default_tags`） |
| `character_evidence.rs` | 候选写成证据批次：每个可替换来源一个批次，覆盖范围 refresh 全库、reindex 改动的图、import 新图与移除的图；版本按行取内容摘要 `c1/<摘要>`，同样的结果复用原证据行 |
| `character_names.rs` | 逐行照 `put_name`、`Store.alias` 写 `names`（名字这一步不收编，方案第 10 节） |

Python 侧：`tests/reference/web/terms_characters.py（#9 已删除）` 在写锁内用只读连接准备同步、刷新、重扫的输入，查询与顺序照旧；`tests/reference/web/terms_core.py（#9 已删除）` 加了 `character_owns`（与 Rust `owns` 同一规则）、`legacy_before`（拆开核心步骤的信封）、`character_command`；`names.action_label` 从信封里读快照，改名后历史文案与旧版相同（计入等价验收，见下）。`Store._refresh_duplicates` 的计算抽成 `_duplicate_rows`（算法不变），供 Python 先算好重复组交给核心。

**实现里定下来的做法**（方案范围内，没有改规则）：

1. **状态与历史分开写**。兼容表照旧 SQL 写。人工结论同时追加成 `term_verdicts`（挂在这一步上），兼容 `assertions` 等于有效裁决。实体的列、work_review、链接这类状态，写完后按受影响的身份镜像进 `terms` / `term_tree` / `term_links`（`legacy_characters::mirror_state`，映射与 S2 相同）。候选只来自证据批次：兼容 `candidates` 按图由有效证据算出（`refresh_candidates`）。
2. **hybrid 下 `assertions` 上的触发器**（`identity.activate` 建的）由核心在写结论的同一段代码里执行（`scope_on_insert` / `scope_on_delete`，含「有行才触发删除」的细节）。双跑时核心那一份先删掉这些触发器，模拟切换后的样子。
3. **名字相关的字符串由调用方给**：名字的语言（`lang_of`）、作品和 tag 的可读名（`readable`）、标成原创角色时的当前显示名、历史文案、`create_linked` 的别名是否与 tag 相同。数据库里的判断（tag 归属、作品查找、成环、上锁、父角色是否被移出）核心在自己的事务里重做。
4. **新身份的 id** 可以由调用方给（双跑时取旧版建出来的那个），不给就由核心生成 UUID。
5. **策略未设置时，兼容 `candidates` 沿破坏式合并解析到代表**（与 `_candidate`、`merge` 写旧表的结果相同）。撤销合并时，按旧版 `_undo_merge` 对候选的改法算出这几张图应有的行；与证据算出的不同（合并发生在收编之前，旧表里只有搬到目标的行），就为这几张图另起一个证据批次（`undo-merge/<步骤>`）。
6. **相册导入**进 album 批次，锚点 = 当时 `actions` 里最大的 id；照旧只在没有结论时写（`INSERT OR IGNORE` 真的写了才追加裁决）。
7. **统一撤销的归属**（`character_commands::owns`）：
   - 新步骤是恰好一项 `{"terms":{"v":2,"type":…,"before":[旧形状…]}}`；
   - 旧步骤的每一项都是角色形状（快照、`merge`、`identity_relation`、`identity_anchor`、`profile`）；
   - 角色形状与别的形状混在一步里，或者形状不认识，拒绝；`[]`、`curation`、`identity_skip`、标签、画师仍归各自的主人。
   - 快照撤销后，兼容行 = 有效裁决（与旧版按快照 DELETE + INSERT 的结果相同），再按快照恢复 deferred 和 scope / suspended / uncertain。
8. **角色切换的前提：标签已切到核心**。这样 `general_hits`、`asset_rating` 是视图，导入时不必另写；S4 切换时检查这一条。

**验收**（双跑框架 `tests/terms_dual.py`：同一个库拷两份，一份旧 Python、一份核心，执行同一串操作；两种策略各跑一遍）：

- **每步之后逐表相等**：兼容表、派生表、私有状态、`names`、`prefs`、`duplicates`、`assets`、`auto_rules`、`excluded`、`imported`、`sync_added`、`curation`、`backend_effects`、身份相关的 `meta`；历史拆开信封后快照、文案、撤销标记、时间都相同，按当前名字重新渲染的文案（`action_label`）也相同。
- **增量与全量一致**：每一步之后 `verify-characters` 从 `term_*` 全量重建兼容表，零差异；派生表按原 SQL 全量重算，等于增量结果；`term_projection` 人物、皮肤那一片，增量等于整表重算。
- **连续撤销等价**：
  - `tests/test_terms_character_history.py` 在收编之前用旧代码留下一串历史：相册之上的否定、撤销后才出现的相册成员、零成员 sweep、资料、跳过、夹在中间的 Python 步骤 `curate`、策略未设置时的破坏式合并；hybrid 下再加关系决议、未定和锚点。
  - 收编之后两边从最新一步撤到最早一步，每一步都相等；在旧历史之上再加核心的新步骤，同样撤到头。
- **核心中途报错什么都不留**：注入触发器让 `change` 在事务中途失败，库的完整转储前后相同（身份命令另有同样的测试）。
- 测试文件：
  - `test_terms_character_commands.py` 11 项（各种裁决、拒绝、复制品展开、改名后的历史文案、中途失败）；
  - `test_terms_character_structure.py` 8 项（合并、资料、`link_local`、作品编辑、类型、当作标签、`shared_oc`、`create_linked`、作品证据的处理顺序）；
  - `test_terms_character_import.py` 6 项（新图、移出与移回、索引补角色与放弃、默认标签、皮肤层级刷新、hybrid 已知身份不套名字规则）；
  - `test_terms_character_history.py` 4 项。
- **变异检查**（改核心代码一处，看双跑是否失败）：
  - 相册不记裁决、策略未设置时不写 variant、不设默认标签、导入不重算规则、hybrid 下已知身份也套名字规则，都有测试失败；
  - 后三项最初没抓到，补了比较的表和场景才抓到；
  - 其中「hybrid 已知身份」要单独一次没有新身份的刷新才看得出：同一次里有新身份时，hybrid 会重建身份图，按基线把层级写回去，这是旧版本来的行为。
  - 另外，撤掉 `action_label` 的改动，历史文案那项测试失败。
- 全量：`cargo test` 68 + 3 项，clippy 零警告；Python 516 项通过（跳过 1 项，同前）。

**旧版已有的不一致（保留旧行为，已由用户批准；全量一致验收在这两处例外）**：

1. 学习发布：≤ 1e-6 的分数变化只保存认领、不刷新归属（见上面「学习发布」一段，比较器的容差严格限于这种情况）。
2. 撤销身份合并时，`identity_claims` 拆组新增的未定图片没有补进刷新范围，增量归属里留着一行、全量重算没有（见上面「身份图」一段，`test_split_scopes_and_flat_or_object_claims_are_preserved_exactly` 钉住这唯一的差异）。

这一组没有发现新的不一致：其余所有命令、撤销、导入之后，派生表的全量重算都与增量结果相等。

**性能（只测量）**：
- 合成库，规模接近真实库：48,000 张图、7,000 个身份、110,277 条候选、59,352 条归属、1,918 条重复。先用旧代码全量刷新，交 WD、切换标签，再做角色收编（4.3 秒，零差异）。
- 然后在两份拷贝上执行同一串命令。核心是 release 构建、经命令行调用（含进程启动），Python 是旧代码直接调用；单次，本机。

| 命令 | 旧 Python（ms） | 核心（ms） |
|---|---|---|
| change 1 张 | 100 | 28 |
| change 50 张 | 191 | 135 |
| sweep（21 张） | 169 | 96 |
| 撤销 sweep | 176 | 229 |
| set_profile 改名 | 46 | 10 |
| merge（破坏式） | 175 | 304 |
| 撤销 merge | 130 | 307 |
| reindex 200 张（Python 准备输入 68 ms 另计） | 581 | 1151 |
| import 20 张新图（含交 WD） | 517 | 697 |
| refresh 全量（含交 WD、重算派生） | 7517 | 15533 |

- 这串命令之后，库的体积：旧 147.5 MB，核心 173.9 MB。多出的主要是第一次 refresh 把全部候选另起了一版证据（收编时的迁移批次版本是整表摘要，refresh 的是按行摘要），之后内容不变的 refresh 复用原证据行。
- 一次 `verify-characters`（全量重建兼容表并比对）707 ms。
- refresh、reindex、merge 比旧版慢。没有做任何优化（按约定只测量），也没有发现平方级的错误；可能的去处（逐行写证据、镜像全部身份、有效集整表重建两次）留给之后专门的优化，真实数字在 S5 副本上测。

**留给 S4**：
- 切换命令，含前置检查：标签已在核心、身份策略、S2 的预检；
- 兼容表在切换时怎么处理：外键、写保护、`links` 用视图还是表；
- Python 各写入口的分支；进程写锁；
- `store.rs::owns` 和 Python `Store.undo` 路由到角色撤销；
- 离线工具在副作用之前拒绝；`_migrate` 跳过核心的表；
- 把 S0 夹具的 `switch()` 接上收编与切换。

### main 整合（2026-09-29，完成）

合入 `main@7332d2f`（合并提交 `4e608ea`）。只有 `tests/reference/web/artists.py（#9 已删除）` 一处文本冲突：两边都保留，terms-core 的 `_posts()` 与核心读路径，加上 main 的「也许是 TA」。main 结尾那行 `maybe` 过滤用的 `seen` 已被帖子的站点集合重新赋值，实际上不过滤；候选在前面已按画师去重，所以照实际效果返回。之后按「当前恢复入口」的四项门槛补兼容：

1. **新方法的强弱**：溯源写 `image_artists.status` 的规则由方法决定。`auto` 是 pixel_hash、gallery_match、iqdb、same_picture；`candidate` 是 ocr 和 group_*。核心画师判定器的强证据加上 `iqdb`、`same_picture`（`schema.rs` Kind::Artist，`tests/reference/web/artists.py（#9 已删除）` STRONG 同步），其余方法照旧只到「也许是 TA」。
2. **候选失效**：schema 第 3 版，溯源证据整个来源按批次生效（`REPLACEABLE_SOURCES`，批次方法记为 `*`）。
   - 证据包按快照替换：包里标为「已完成」的图是覆盖范围，覆盖到而没有行就是查无结果（与包格式原有的约定一致）；失败、取消的图不覆盖。
   - `tests/reference/provenance/export.py（#9 已删除）` 把图库里每张图都列为「已完成」，每个包是真正的全量；内容相同的快照照旧去重，不另起批次。
   - 画师收编（切换前的同步、切换）每次也写一个全库覆盖的批次，内容不变不新建。
   - 迁移时已有的溯源证据全部收进一个初始批次，迁移前后有效的证据不变。
   - v3 要放宽 `term_evidence_sets` 的 CHECK，父表和三张子表一起重建（新子表的外键指向新父表，再用新式 ALTER 改名），失败时回到 v2 原样。
3. **读路径**：切换后 `for_asset` 返回 `maybe`（投影为「也许是」、没人判断过的画师，原因取当前有效证据里分数最高的一条，文案与 main 相同），`methods` 列出当前有效证据的全部方法，抽查池只看当前有效的强证据。
4. **生命周期**：每次运行结束（正常、取消、失败）都交一次包，按需查找走同一条路径，启动时补交一次（main 与 terms-core 的接线合并后都在）。分组推荐读的 `duplicates`、`similar_groups`、`memberships`、`entities`、`clusters` 在角色切换后仍是同名表，只读。

**已知差别（唯一）**：用户已确认的候选，如果分组推荐重算后不再提出，main 的旧读法里这张图的这位画师会消失（旧读法只在 `image_artists` 的行上叠加人工判断），核心里人工的「是」继续有效。按门槛「机器证据更新不得抹掉人工裁决」和核心「裁决先于证据」的原则取核心的做法，由 `test_a_confirmed_candidate_stays_when_the_machine_stops_offering_it` 单独钉住。

**验收**：`tests/test_terms_artists_provenance.py`。对照标准是 main 的旧读法直接作用在当前的 provenance.sqlite 与当前人工判断上，切换前后、每次交付后逐张图、逐位画师比较：画师、方法、数量、确认 / 否定、帖子、候选及原因、`assets_of`。场景包括 IQDB 与同一张图的认定、OCR / 分组候选、重复交付、候选重算后消失、分数变化、切换后对候选说是、对 IQDB 认定说不是、撤销、查无结果。

变异检查：导出只列有行的图，或者核心不认 IQDB 为强证据，都有测试失败。Rust 新增：v2→v3 迁移有效证据不变且失败整体回滚；溯源包按快照替换、人的裁决不受影响。

**全量**：cargo test 70 + 3，clippy 零警告，Python 581 项通过（跳过 1 项，同前）。S4 原剩余项仍待完成（见上一段）。

### S5 总结（2026-09-29）

**结论：全部验收项零差异。** 允许的例外只有以下已批准或已确认的几条，每条都有单独的检测，没有扩大比较范围。

| 验收项 | 结果 |
|---|---|
| 影子收编 `sync-characters` | 10 张兼容表 0 差异；出处无法解释 0；重跑不新增行；6 处故意破坏全部报出并回滚 |
| 真实历史连续撤销 50 步 | 每步逐表相等，`verify-characters` 每步零差异；「增量 ≠ 全量」没有新增 |
| 抽样操作双跑 | 27 种操作 + 撤销 8 步，35 步全部逐表相等 |
| 切换后三方对照 | 旧读路径、核心、App 读路径全部 0 差异（约 1.7 万个接口调用、画师 19,412 张 / 6,078 位 / 抽查池 17,442 张）；3 处故意破坏全部报出 |

**实现 bug（已修，都有测试钉住）**：
1. 收编预检按列的顺序比较，真实库列序由 `ALTER TABLE` 历史决定，于是被拒绝。改成按列名集合比较（第 1 部分）。
2. 切换时没有给人物 / 皮肤建 `term_projection` 镜像，切换后这一片是空的，只有之后的命令按图增量维护。改成切换事务里整片镜像一次（第 2 部分）。

**差异清单**：
1. 撤销画师 / 标签步骤时，旧版多排一条 `verdict` 效果，切换后不排（第 2 步起的设计）。**用户 2026-09-29 确认保留。**
2. 学习发布时 ≤1e-6 的分数变化不刷新归属（S3 已批准的旧行为）：真实库里有 1 张图，保留原样，镜像也照增量结果。
3. 表示方式的变化（非行为）：画师步骤的历史快照写成 `[{"terms":"verdicts"}]`，同第 2 步。

**真实数据里的新情况**：
- 身份策略是 hybrid；
- `entities` 的列序与新建库不同；
- `wd` 链接只有 30 条；
- 相册导入出现在历史中间（锚点到 1,156）；
- 激活前的 `merge` 只有 1 步；
- 真实库还没有切换过画师 / 标签（一直在跑 main），所以 App 第一次启动时会依次做三次切换，都在副本上验证过；
- 画师数量从第 2 步时的 4,011 位涨到 6,078 位，有 2,298 张图带「也许是 TA」。

**性能表**（真实规模，单次，只测量未优化；切换后含 App 路由、先演一遍旧代码和核心进程启动）：

| 项 | 旧 | 切换后 | 倍数 |
|---|---|---|---|
| 首次启动的切换（画师 + 交包 + 标签 + 角色） | — | 134 秒（标签 115 秒在后台） | — |
| 单张的改 / 设为 / 移出 | 5–150 ms | 23–125 ms | 约 +20–70 ms |
| sweep | 159 ms | 333 ms | 2.1× |
| 撤销（47 个角色步骤合计） | 3.2 秒 | 5.1 秒 | 1.6× |
| 关系决议（一般） / 合并 | 384 / 432 ms | 1,326 / 1,480 ms | 3.4–3.5× |
| 关系决议（影响 5,331 张） | 1.5 秒 | 11.7 秒 | 7.8× |
| 新建角色 | 2.6 秒 | 7.2 秒 | 2.7× |
| reindex 200 张 / 移出 20 / 导入 20 | 1.0 / 0.8 / 0.8 秒 | 2.7 / 1.5 / 1.9 秒 | 2–2.8× |
| 全量 refresh | 15–28 秒 | 63–76 秒 | 2.7–4× |
| 读路径（同一批约 1.7 万次调用） | 322 秒 | 387 秒 | 1.2× |
| `links` 关键查询 | — | 计划相同、耗时相同 | 1× |
| library.sqlite | 451.5 MB | 1,098.6 MB（检查点后；切换中最大 1,360 MB + WAL 262 MB） | 2.4× |

**是否具备回合 main 的条件**：从等价性看具备：S0–S5 的验收项全部通过，发现的两个实现 bug 已修复并有测试，全量回归通过。以下几点需要你另行决定，本次都没有做：
- 性能：交互操作多出几十毫秒可以接受；关系决议、合并、refresh 慢 2.7–7.8 倍，大关系决议要 11.7 秒。要不要先优化再回合（按约定这里只测量）。
- 体积：库从 452 MB 涨到约 1.1 GB（主要是 257 万条 WD 证据），切换过程中需要约 1.6 GB 空间，外加三份自动备份各约 0.45–1.1 GB。
- 切换是单向的：回合后第一次启动就会在真实图库上切换，main 的旧代码之后读不到切换后的数据；回退只能恢复切换前的自动备份。
- S6 文档（`docs/TERMS_CHARACTERS.md`、TERMS_SCHEMA 第 5 节的更正）还没写；Windows 未验收。

S5 没有合并 main，没有改主目录，没有启动真实图库。副本在测试结束后删除。

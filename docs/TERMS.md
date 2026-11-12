# 词条：产品定位与架构收敛方案

状态：现行参考与后续方向，2026-11-12。在 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md) 的底线基础上说明词条抽象、现行设计与用户批准的演进范围。第 3、4 节是收敛的理由和已完成的收编顺序（各步结果见对应文档），剩余方向见第 5 节。

## 1. 产品定位

**二次元收藏的相册应用。** 沿用消费级相册（Apple Photos、Google Photos）的范式：保存即入库，结构自动长出来，用户只负责享用；用在大厂相册认不出、云端又不适合托管的内容上。

收藏的性质决定了约束（依据见 [PROVENANCE.md](PROVENANCE.md) 摸底和 [BACKLOG.md](BACKLOG.md)）：图是手机上长按保存的，文件名被 iOS 改掉、EXIF 没有来源；用户不用 Pixiv 收藏；按段保存（Pixiv 平均每段 7.6 张）；每天约 40 张；仅 `1girl+1boy` 一类约一万张、九成 R18。由此：用户不整理，上下文由机器补，只能本地，结构自己长。

**北极星是用户回到收藏里的频率**（找图、逛、去画师主页）。正确率是护栏，按界面定标准：逛的界面只放高把握的，画师宁缺毋滥，搜索可以放宽，拿不准的进「也许是 TA」。抽查、校准、评测等研发台保留，退出日常界面；日常界面不出现待办数字。

### 与 Monbooru 的区别

[Monbooru](https://github.com/monbooru/monbooru)（v1.21.1 核对）是档案工具范式：以标签组织通用媒体，用户是管理员（词表、批量操作、搜索语法、Docker 部署）。两边都用 WD，这是技术上的重合。

- 标签聚合（`internal/tagger/aggregation.go`）：均值过阈值，按类别 top-K（character 8），没有第一名与第二名的分差，没有弃权。
- 重跑（`internal/tagger/tagger.go` 约 500–590 行）只删改 `is_auto=1` 的行，新结果 `INSERT OR IGNORE`：用户删掉的自动标签会被贴回。
- 它在关系模块有 `not_related_pairs`（`internal/db/schema.sql:323`），被拒绝的配对永不再出现；标签上没有对应机制。

值得借的是「一切皆标签、可自由组合」的交互；我们补上它缺的一半：记住否定、按场景控制正确率的裁决引擎。

## 2. 核心抽象：词条

整个产品围绕一个概念：**词条**。角色、皮肤、作品、画师、分级、题材、用户自建的概念、重复组、保存段，都是词条。往图上加词条叫「贴」，机器贴、用户贴都一样。代码里可以继续叫 `tag`。

**一个对象、两种关系、一条规则：**

- **词条**：类型（`kind`）只是属性，自带多语言名字（沿用 names 层）和外部链接（Danbooru tag、Pixiv 用户 ID、Bangumi 条目，沿用 links 层）。改类型只改这一个属性。
- **图—词条**：来源就是谁贴的。WD、CCIP、像素哈希、OCR、规则、例子学出的模型、分享链接、用户本人都是来源，带版本号。
- **词条—词条**：包含（皮肤 → 角色 → 作品）、等同（别名、合并）、互斥（单人图只能一个角色），以及从数据算出的共现。
- **规则**：证据只增不删，一个来源不覆盖另一个来源；裁决只来自人，否定永久有效，带 `action_id` 可撤销；图上显示什么由证据、裁决和类型策略算出（人工确认 / 机器认定 / 也可能是 / 已否定），各界面按自己的标准取档。

**差异放进类型策略，不拆表：** 互斥与计数（单人图互斥、分级恰好一个）、判定器（角色：d1/d2 校准加单人规则；题材：规则或阈值；画师：只收强证据）、默认界面标准、是否参与提问。「只当标签」只是改 `kind`；原创角色是用户新建的角色类型词条；CP 是新类型加自己的判定器。

标签挂在去重后的「作品」上，不挂在单个文件上（现有写操作已按重复组扩展）。分组算法（`duplicates`、`similar_groups`、`clusters`）留在外围，结果以系统生成的组类型词条进入核心。

### 在词条上能长出的东西

- **贴**：词条可以从规则起步（「夏日」= swimsuit / beach / watermelon 任一），从例子学（五张图 + `pi.db.embeddings` 里已有的 WD 1024 维特征），或由人贴；按一下「不算」永不贴回。
- **连**：两个角色常一起出现就是 CP；画师 × 角色就是「TA 画的 mika」；贴得最多、最久的就是本命。
- **看**：每个词条一页；首页是最重要的几个词条，搜索是词条组合，时间线是词条随时间的变化，分享卡片是「图 + 画师词条」。

## 3. 现状的冗余

收敛前（2026-09-28）的冗余在概念上：同一套角色在角色（hybrid）、身份 v2、画师溯源、用户标签四条线里各实现一遍，分布在 3 个数据库、约 80 张表。下表是当时的状况，现行表见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md)。

| 词条模型的角色 | 应有 | 现状 |
|---|---|---|
| 词条（含名字、链接） | 1 套 | `entities`/`links`/`names` · `identity_nodes` · `artists`/`artist_links`/`artist_names` · `display_names`/`name_aliases`/`name_cache` |
| 证据 | 1 张表 | `candidates` · `weak_claims` · `proto_claims` · `family_evidence` · `general_hits` · `identity_evidence` · `image_artists` · `leads` |
| 裁决 | 1 张表 | `assertions` · `asset_tags` · `artist_verdicts` · `identity_decisions` · `identity_anchor_decisions` · `work_review` · `reviews` · `curation`/`excluded`/`deferred` |
| 投影 | 1 张物化表 | `memberships` · `tag_memberships` · `variant_memberships` · `identity_claims` · `identity_assignments` · `ambiguous_claims` · `asset_rating` · `single_assets`/`lead_assets` |
| 词条关系 | 1 张表 | `relations` · `identity_relations` · `identity_canonical` |

撤销历史 `actions` 已经统一，保留。

三处主要冗余：

1. **三套词条注册表、三套名字。** 画师作为一种词条后，溯源的 `artists` 一组表并入核心，`artist_verdicts` 并入裁决表；人工决议归核心，溯源 worker 只交证据包（已完成，见 [TERMS_ARTISTS.md](TERMS_ARTISTS.md)）。
2. **两套身份系统并存。** 生产跑 hybrid；v2 的 15 张 `identity_*` 表建好后从未启用，2026-09-30 用户决定放弃 v2，表已删除（第 5 节）。v2 的证据 / 决议 / 决议所审证据 / 可撤销关系分层是词条核心的原型，应推广到所有类型；锚点决议、`identity_scopes`、`identity_pairs`、`identity_uncertain` 降为角色判定器的私有状态。
3. **每种机器贴法一张表。** 统一成一张证据表后，新来源只是多一个 `source` 值；`single_assets`、`lead_assets`、`asset_rating` 是 WD 证据上的规则投影。

**留在外围、不算冗余：** `pi.db` 原始标签和特征；溯源的 `marks`、`lookups`、`ref_sigs`、`ext_cache`、`seeds`、`runs`；校准产物 `identity_calibration`、`audit_sample`；任务状态 `sync_runs`、`pending_runs`、`backend_effects`。`asked` 是提问日志，答案写进裁决表。

## 4. 迁移方案

### 冻结分支 `codex/rust-migration`（`5515f6e`）

不恢复、不合并，原样保留并打归档 tag。它借旧连接按原模型迁移表结构，按词条模型这些表大半要收掉；其中不依赖表结构的纯逻辑（作品层级闭包与成环处理、名称选择顺序、历史文案渲染、跨模块撤销分派）作为规格移植到新 schema。未合并的 `codex/research-boundary`（worker 契约）并入第 1 步的证据包格式。

### 顺序

词条核心是 Rust crate `crates/core`（`illustash-core`，由应用宿主 `native/terms` 使用），用 rusqlite 自己持有连接；收编全程前端 API 不变。五步均已完成：

1. **定核心 schema 和证据包契约。** 核心约八张表：词条、名字/链接、词条关系、证据、裁决、投影、`actions`、资产。以 v2 分层为蓝本，`kind` 扩到所有类型。[RESULT_PACKAGES.md](RESULT_PACKAGES.md) 的结果包协议与 `codex/research-boundary` 的 worker 契约合并为一份证据包格式：图、词条 key、来源、版本、分数、证据 JSON。
   schema、迁移与规则见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md)，证据包格式见 [EVIDENCE_PACKAGES.md](EVIDENCE_PACKAGES.md)，代码在 `crates/core/`。HTTP、前端调用链与默认原生入口由同一应用核心接管，现行边界见架构文档。
2. **画师词条（第一个闭环）。** 溯源最接近词条模型，而且证据和裁决在代码里已经分开（[PROVENANCE.md](PROVENANCE.md) 第 6 节的草案与实现不同，以代码为准）：`provenance.sqlite` 的 `image_artists.status` 只有 `auto`、`candidate`，都转为证据——`auto` 来自像素哈希、本地比对，`candidate` 来自 OCR，正好对应画师判定器的强、弱证据；`image_artists` 每对 (图, 画师) 只有一行，多种方法命中时只留一种（像素哈希 `INSERT OR REPLACE` 覆盖，本地比对遇到已有行不写，OCR 只更新候选行），被挤掉的方法无法从表里恢复；收编后每种方法各是一条证据，重跑 worker 可补齐。人工的「是 TA / 不是」已经在 `library.sqlite` 的 `artist_verdicts`（`yes` / `no`）里，进统一的 `actions` 历史、可以撤销，收编为 `term_verdicts` 并沿用原来的 `action_id`。草案里的 `confirmed`、`rejected` 状态和 `prov_actions` 表没有实现，代码里不存在，不需要迁移。选它先做：模块刚完成、归属待定、判定器最简单、与角色无耦合、已有 48/48 用户验收可对照。界面上的「是 TA / 不是」「画师 · 主页 · 库里 N 张」改由核心提供。`provenance.sqlite` 只留研究缓存，溯源 worker 只产出证据包。结果见 [TERMS_ARTISTS.md](TERMS_ARTISTS.md)。
3. **用户标签、分级、WD 题材。** 判定器简单；`general_hits` 转为 WD 来源的证据。
   WD 分级与题材以证据包交核心，用户标签成为用户概念。结果见 [TERMS_LABELS.md](TERMS_LABELS.md)。
   **2026-09-28：用户决定第 4、5 步合并做**（作品、名字与角色同在 `entities`，规则互相依赖）。
4. **作品和名字。** 作品树在 `term_tree`，规则由核心原样执行（[TERMS_CHARACTERS.md](TERMS_CHARACTERS.md)）。名字的选名规则进核心，算成显示投影，每个写事务同步维护；名字主数据、搜索和历史文案都在核心（[TERMS_NAMES.md](TERMS_NAMES.md)）。
5. **角色（最后）。** 按 D1 只支持身份策略未设置和 hybrid（v2 放弃），原有规则由核心原样执行：身份图在 `crates/core/src/character_graph.rs`，没有采用冻结分支的新规则。HTTP 服务、读路径和写入口都在 Rust 应用宿主，只维护一套身份系统。结果见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md)。

收编的做法是先投影后收编：先在旧表上建映射到新 schema 的只读视图，切读路径并逐表对照，再收编写路径。旧表在核心第 8 版（#201）删除或定为核心维护的派生表，见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13、14 节。

### 验收

每一步按 ARCHITECTURE_DIRECTION「恢复条件与验收标准」：数据写入归属明确；读、写、历史、撤销端到端走通；失败无部分提交；真实库副本逐表对照；性能基线；可执行回退路径。完成后核心约十张表，外围是 worker 私有的缓存和研究表，两边只通过证据包交接。

## 5. 剩余工作

词条核心的收编（画师、标签、角色 / 皮肤 / 作品、名字）已全部完成。核心第 8 版（#201）起 `term_*` 是唯一权威：兼容表删除，或定为核心维护的派生表（`assertions`、`candidates`、归属表），收编与切换的机制也已删除，见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13、14 节。HTTP 读路径由原生宿主直接组装，见 [ARCHITECTURE_DIRECTION.md 的 HTTP 读路径](ARCHITECTURE_DIRECTION.md#http-读路径2026-09-30)。

**用户的决定（2026-09-30）**：
- **身份策略 v2 彻底放弃**：核心按 D1 只支持策略未设置和 hybrid，v2 从没启用过，校准门槛也没达到；v2 专用表在核心第 7 版删除。
- 名字那一步里唯一的行为差别（非法 `work` 参数：500 改为 400）接受。

还没做的是**新类型的判定器**（只在核心里加东西）：

- 「只当标签」改成正式类型：现在由吉祥物标记和 `tag_memberships` 实现；
- CP（`pair`）；
- 系统生成的组类型（重复组、相似组、聚类、保存段），分组算法留在外围，结果以组类型词条进核心。

性能现状见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)。

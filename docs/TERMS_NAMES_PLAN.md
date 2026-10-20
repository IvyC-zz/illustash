# 名字投影与收编方案（词条第 4 步的名字这一半）

状态：已完成的方案，留作代码注释按节号引用的设计记录。其中「旧表成为由核心维护的兼容表」的部分已被词条核心第 8 版（#201，2026-10-20）取代：兼容表删除或定为核心维护的派生表，收编与切换命令删除，现状见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13、14 节。

2026-09-29 起草；**2026-09-29 用户确认第 5 节**（N1 改为「这一步就搬进核心」，N2–N4 按建议）。前置契约见 [TERMS_CHARACTERS_PLAN.md](TERMS_CHARACTERS_PLAN.md) 第 10 节和审核意见 B5（`TERMS_CHARACTERS_REVIEW.md`，已删除，见开源前私有历史）。分支 `terms-scaffold`，做完验收后合回 main。Python 测试 2026-10-07 起已删除（#9）：文中提到的 `tests/test_*.py` 改由 `native/terms/tests/replay.rs` 中同名模块的录制重放及 Rust 单元测试覆盖，现状见 [TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 第 6 节与 [TERMS_NAMES.md](TERMS_NAMES.md) 第 6 节。

## 1. 现状

**名字数据**（都在 library.sqlite，Python 的表）：

| 表 | 内容 | 谁写 |
|---|---|---|
| `names` | 按身份的名字：`entity_id, lang, value, source, role(display/alias), preferred, position` | 用户（改名、别名，经角色核心命令按 `put_name` 规则写）、NameUpdater（Bangumi / Danbooru / 模型翻译）、合并、作品编辑 |
| `display_names` | 按文字键的名字：`(kind, key)` = 作品的 qualifier、皮肤的 variant | NameUpdater（`names.put`） |
| `name_aliases` | 按文字键的别名（搜索用） | NameUpdater |
| `name_cache`、`name_settings` | 联网结果缓存、模型设置 | NameUpdater（外围，不算名字数据） |

**选名规则**全在 `tests/reference/web/names.py（#9 已删除）` 的 `View`：
- `choose(kind, key)`：按首选、来源优先级 `user > bangumi > index > danbooru > wiki > builtin > model` 选每种语言的名字，再在语言之间选；皮肤词表 `SKINS`、作品词表 `WORK_NAMES` 的例外；英文回退到可读的 tag。
- `entity(ident, skin_only)`：作品（用户名字 → qualifier 的词表名 → 自己的名字）、本地 / 原创角色只用自己起的名字、皮肤（皮肤名，或「以前的人物变成的皮肤」保留自己的名字；完整名是「父 · 皮肤」）、人物（`base`：合并的代表借用成员的名字）。
- 产出：显示名、名字来源、zh / ja / en 三种语言、tag。

**读者**：
- 注册成 SQL 函数 `entity_name()`、`entity_info()`、`work_name()`，嵌在 34 处查询里（角色列表、作品列表、搜索排序、详情、历史文案……）；
- `Store._display_reader`（批量显示）、`names.action_label`（历史文案按当前名字重新渲染）直接用 `View`；
- `names.search_hits`：搜索，直接读三张名字表、词表和作品树。

快照按 `meta.names_version` 共享（`entities`、`links`、`names`、`display_names` 上的触发器递增它）。

## 2. 目标

1. **选名规则进核心**：核心按与 `View` 逐条相同的规则算出**显示投影**，在每个写事务里增量维护；Python 的全部显示读路径改读投影。
2. **名字主数据进核心**：`names`、`display_names`、`name_aliases` 收编为核心的主数据，旧表变成由核心维护、加写保护的兼容表（与角色那一步相同）；NameUpdater 仍是 Python 的联网 worker，只交结果给核心，不再直接写表。
3. **完成拆脚手架阶段 C 里依赖名字的部分**：历史文案由核心生成；剩下的写入口去掉「先演一遍」。

**不变**：界面、HTTP API、选名结果、搜索结果、历史文案，逐字不变（等价迁移）。

## 3. 设计要点

- **显示投影表**（核心维护，物化）：
  - 按身份一行：`完整显示名、皮肤短名（skin_only）、名字来源、zh / ja / en、tag`；
  - 按文字键一行：`(work, qualifier)`、`(skin, variant)` 的 `choose` 结果，供 `work_name(tag)` 和皮肤名用。
- **失效依赖**（投影要随之重算的变化）：身份的属性（kind、original、parent、variant、merged_into）、当前链接、名字行、文字键名字、词表。~~按受影响的身份增量重算~~ **（N1 实测后改）整表重建**：8,003 个身份的整表重建约 40 ms（含进程启动），比 Python 装载一遍 `View` 再算完全部身份（70 ms）还快。所以凡是改到名字相关数据的核心写事务，都在同一事务里整表重建；判断依据是 `meta.names_version`（`entities`、`links`、`names`、`display_names` 上已有的触发器会递增它）。「增量等于全量」由构造保证，不需要依赖推算。
- **词表**：`SKINS`、`WORK_NAMES`、`readable()`、`skin_zh()` 移植进核心。Python 里的副本仍给搜索和 NameUpdater 用，用一致性测试钉住两边相同。
- **Python 读路径**：`entity_name()` 等三个 SQL 函数改成读投影（函数名不变，34 处查询一行不改）；`_display_reader`、`action_label` 改读投影。
- **搜索进核心**（N1）：`search_hits` 的匹配与排名规则移植进核心，Python 的搜索入口改调核心；结果（身份 → 排名）逐字相同。
- **写路径**：用户名字早已经过核心命令；NameUpdater 的结果改为交给核心（第 5 节 N2），按来源整批替换，成功的空结果让旧名字失效；合并、作品编辑写名字的地方都已在核心命令里。
- **切换**：与前几步相同。启动时在后台做：最后一次同步、零差异核对、兼容表加写保护、写 `terms_names=core`；失败留在旧表，下次启动再试；切换前先备份。

## 4. 分段计划与验收

| 段 | 内容 | 验收 |
|---|---|---|
| N0 夹具 | 在旧代码上钉住选名行为：qualifier 作品读 `display_names`、词表例外、英文回退、合并代表借名、本地 / 原创只用自己的名字、皮肤名与「以前的人物变成的皮肤」、作品的用户名字、`put` / `put_name` / 替换的写入规则、`action_label` 的重新渲染 | 全部在旧代码上通过；之后原样用于新代码 |
| N1 投影 | 核心按 `View` 规则建投影，先从 Python 的名字表只读地算（影子），不接读路径 | 所有身份、所有文字键，投影与 `View.entity`（两种 skin_only）、`View.choose` **全量逐字相等**；增量等于整表重算；故意破坏一处能报出 |
| N2 名字收编（影子） | `sync-names`：名字三表收编进核心主数据，由主数据生成影子兼容表逐行比对 | 零差异；再跑不新增；预检失败整次回滚 |
| N3 核心写名字 | NameUpdater 的交付、所有写名字的入口改走核心；核心命令同事务维护投影 | 双跑：每步兼容表、投影、历史逐表相等；连续撤销等价 |
| N4 切换与读路径 | `cutover-names`；三个 SQL 函数、`_display_reader`、`action_label` 改读投影；`search_hits` 改调核心的搜索；启动时在后台切换 | 切换前后所有读接口结果相同，搜索与 `search_hits` 全量对照（名字、别名、tag、作品树、皮肤词、「父 · 皮肤」组合查询）；写保护、失败留旧表、重启恢复 |
| N5 历史文案进核心、拆掉剩下的预演 | 历史文案由核心按投影生成；`set_profile`、`link_local`、日常命令、策略未设置的合并去掉预演 | 拒绝对照、双跑零差异 |
| N6 真实副本验收（**需授权**）与文档 | 在一致性副本上重复 N1–N5，并做三方对照（旧 `View`、核心投影、App 读路径）和性能测量 | 零差异；性能只测量 |

## 5. 要确认的决定

| # | 问题 | 建议 | 理由 |
|---|---|---|---|
| N1 | 搜索（`search_hits`）这一步是否进核心 | 建议留在 Python。**用户决定：这一步就搬进核心** | 它只做字符串匹配，没有选名规则；用户希望一并离开 Python |
| N2 | NameUpdater 怎么把结果交给核心 | **核心命令**（像角色命令一样，标准输入给 JSON：按来源整批替换、成功的空结果、display / alias / 首选 / 顺序），不走证据包文件 | 名字是显示数据，不参与归属判定；证据包的格式和按指纹去重是为机器证据设计的，名字用不上，反而要大改包格式 |
| N3 | 名字在核心里怎么存 | **按现在的形状收进 `term_names`**（来源、语言、角色、首选、顺序），用户名字就是来源 `user`，不拆成证据和裁决 | 选名规则本来就是按来源排优先级；拆成证据 / 裁决不改变任何结果，只增加复杂度 |
| N4 | 拆脚手架阶段 C 里依赖名字的部分（历史文案进核心、剩下的写入口去掉预演）放在这一步做吗 | **放在这一步的 N5 做** | 名字规则进了核心之后顺手就能做；拆开做要再对照一遍 |

## 6. 实施进度

### N0 夹具（2026-09-29，完成）

- `tests/terms_names_fixture.py`：一个把选名规则每条分支都走到的小图库（在 `tests/test_web.py` 的夹具上直接写旧表），加上全量快照 `snapshot(c)`：每个身份的 `View.entity`（完整名与 skin_only）、每个文字键（作品 qualifier、皮肤 variant、角色 tag）的 `View.choose`、22 个查询的 `search_hits`、全部历史的 `action_label`。N1 起核心的结果与它逐字对照。
- `tests/test_terms_names_contract.py`（6 项，旧代码上通过）钉住的行为：
  - **作品**：词表压过 bangumi；用户中文名压过词表；机翻中文排在 index 日文之后；wiki 中文按 bangumi 排；可读 tag；作品自己的用户名字先于 qualifier；没有 qualifier 用自己的名字。
  - **人物**：首选先于来源；英文不显示、回退到去掉 qualifier 的可读 tag；合并代表借成员的名字；有用户名字只用自己的名字；本地、原创、未命名；只有 WD 当前链接、退役链接不算。
  - **皮肤**：词表、复合词「新年 · 和服」、以前的人物保留自己的名字、与父同名退回可读名、父名是机翻时整名标 model、本地角色的皮肤不拼父名。
  - **历史文案**：四种重新渲染的形状加不认识的文案。
  - **搜索**：名字、别名、tag、退役链接、作品词表、作品树、皮肤词、「父 · 皮肤」组合、排名。
- 夹具起初有两处没走到的分支（皮肤没有链接，全被当成本地角色；词表作品下没有角色），按真实数据的形状补上。
- 变异检查：去掉合并借名、wiki 不再按 bangumi 排、皮肤同名不退回可读名，都有测试失败。

### N1 显示投影（2026-09-29，完成）

- `crates/core/src/names_view.rs`：逐条移植 `View` 和词表。有几处要照搬 Python 的细节：
  - `skin_zh` 按词长排序时，同长的词保持词表原顺序（稳定排序）；
  - `readable` 的去 qualifier 正则、Python `str.title()` 的大小写规则；
  - `min` 取第一个最小值；wiki 的中文按 bangumi 排，只对中文生效。
- schema 第 5 版：`term_name_display(entity, skin_only, …)`、`term_name_choice(kind, key, …)`。命令行 `names-project --db` 整表重建（影子阶段，还没有读者）。
- `tests/test_terms_names_projection.py`：夹具库上投影与旧代码的全量快照逐字相等：每个身份的完整名与皮肤短名、每个文字键的 `choose`，以及投影比快照多出来的键。再做 9 种改动（加用户名字、改首选、拆合并、退役链接、词表作品的用户名字、删皮肤自己的名字、标成原创、皮肤的机翻名与 bangumi 名），每改一处都重建并全量对照。
- 变异检查：去掉「wiki 中文按 bangumi 排」（10 处失败）、去掉「皮肤与父同名退回可读名」（1 处失败）。
- 合成副本（8,003 个身份、24,005 条名字）：整表重建约 40 ms。

### N2 名字收编（影子，2026-09-29，完成）

- `crates/core/src/legacy_names.rs`，命令 `sync-names` / `verify-names`：
  - `names` → `term_names`：只动角色、皮肤、作品（`term_tree` 里的词条）的行，画师和用户概念的名字不碰；
  - `display_names` → `term_key_names`，`name_aliases` → `term_key_aliases`（两张新表并进 schema 第 5 版，第 5 版还没发布）；
  - 过程：只读旧表，按整行比较补齐（多的删、少的加），再由主数据生成的样子与旧表逐行比对；一个事务，预检失败或有差异都整次回滚（退出码 3）。
  - 名字是状态不是历史，没有「出处」要解释。
  - 前提：角色已经切到核心，因为名字挂在角色的词条上。
- 预检：名字属于不在核心里的身份；名字为空或超过 200 字（`term_names` 的约束）；语言或角色不合约定；首选不是 0 / 1。
- `tests/test_terms_names_sync.py`（5 项）：
  - 零差异、再跑不改任何行；
  - 旧表之后的改动（新名字、改首选、删名字、新文字键名字、改别名顺序）同步后仍一致；
  - 改坏核心的三张表之一，`verify-names` 报出；
  - 超长名字被预检拒绝且什么都不留；
  - 角色没切换时拒绝。
- 夹具的调整：用来看文案重新渲染的几条伪造历史标成已撤销。它们不是真实操作，其中合并快照的 `moved` 不是 `[图, 状态, 来源]` 形状，角色收编会拒绝。N0 钉住的文案不受影响。
- 合成副本（24,005 条名字）：首次收编 0.14 秒，再跑 0.08 秒。

### N3 名字写入走核心（2026-09-29，完成；同时做了 N4 的切换命令）

- **提交钩子**：`schema::commit_writing`（所有核心写事务的统一提交点）调用 `legacy_names::after_write`。名字已在核心时，比较 `meta.names_version` 与上次投影时记下的 `names_projected`：变了就在同一事务里补齐名字主数据、整表重建显示投影。日常命令不碰名字表，只多两次 meta 查询。
- **`names-command`**（`crates/core/src/names_commands.rs`）：逐行照 Python 移植 NameUpdater 写库的原语，报错文字相同：
  - `replace`：按来源整批替换，空列表就是成功的空结果；
  - `danbooru_aliases`：人物按 tag 找身份写；作品写 `name_aliases` 并同步到 qualifier 对应的作品身份；
  - `bangumi_alias`；
  - `seed_index`，以及 `seed` 里用到的 `put`；
  - `alias`：`Store.alias`，相册标题用；
  - `batch`：多条放在一个事务里。
- **`cutover-names`**：一个事务里最后一次收编（必须零差异）、名字三表加写保护、写 `terms_names=core`、整表建投影。前提是角色已在核心。服务启动时 `adopt_terms` 按「标签 → 角色 → 名字」切换（`adopt_names`），切前备份，失败留旧表。
- **Python**：`NameUpdater.replace` / `aliases` / `character`（Bangumi 别名）/ `seed` 和 `Store._album_titles` 在名字已切换时改走核心命令，旧路径保留给没切换的库，也继续当双跑的标准答案。核心拒绝时转成 `ValueError`，文字与旧代码相同。
- **测试**：
  - `tests/test_terms_names_writes.py`（4 项）：名字夹具库拷两份（一份不切名字、一份切），两边执行同一串写入：
    - NameUpdater：替换、成功的空结果、作品和皮肤的替换、没有主人的 tag、三种 Danbooru 别名、非法来源两边都拒绝且都不写；
    - 索引种子与相册别名；
    - 改名、新建角色；
    - 每步之后：三张名字表逐行相等，切换那份的投影等于旧 `View` 在同一数据上的结果，`verify-names` 零差异；
    - 另测：切换后 Python 直接写名字表被拒绝，外围表 `name_cache` 照常可写。
  - 切换后再跑已有测试的那一层（`tests/test_terms_switched_suites.py`）改为「标签 → 角色 → 名字」都切：110 项通过。另有 7 项是测试自己在原始连接上直接写名字表来布置数据，按惯例列入不跑清单并注明原因，不是生产代码漏改。
- 变异检查：核心的 `put` 不写作品身份（1 处失败）；投影不随写入重建（3 处失败）。

### N4 读路径与搜索进核心（2026-09-29，完成）

- **开工先合入 main@202dbd5**（`test_net_keepalive` 偶发失败的修复），全量回归通过后再继续。
- **读路径**（`tests/reference/web/names.py（#9 已删除）`）：
  - `ProjectedView`：接口与 `View` 相同（`entity(ident, skin_only)`、`choose(kind, key)`、`load()`、`entities`），读核心的 `term_name_display`、`term_name_choice`；`entities` 仍是 `entity_rows` 的原始身份行（`_display` 要用）。按 `meta.names_version` 在连接之间共享快照，只共享已提交的状态（与 `View._SHARED` 同一做法）。
  - `choose` 遇到投影里没有的键（任意 tag 的 `work_name`、词表里有但库里没有的作品），退回 `View(c).choose`。投影只收库里出现过的键。
  - 与 `View` 一样，还没装载快照时的单次 `choose`（NameUpdater 的后台检查）只按主键读投影的一行，不装载整个图库（`test_translate_with_cached_names_never_loads_full_library` 切换后第一次跑时抓到）。
  - 工厂 `names.view(c)`：名字已在核心给 `ProjectedView`，否则 `View`。**例外**：写入口的预演连接（`terms_route.PlanConnection` 且 `captured` 不为 None）仍给 `View`——预演会先改名、再用新名字拼文案，预演事务里投影还没重建。N5 拆掉预演后去掉。
  - 改用工厂的地方：`register`（`entity_name` / `entity_info` / `work_name`，经它也覆盖 `Store._display_reader`）、`action_label`、`NameUpdater.info` / `character` / `translate`。
- **搜索进核心**（`crates/core/src/names_search.rs`，命令 `names-search`，标准输入 `{"query": …}`，输出 `{"hits": {身份: 排名}}`）：
  - 逐条移植 `search_hits`：`LIKE` 预筛（SQL 语句逐字照搬，所以只对 ASCII 不分大小写这一点也相同）、Python `str.casefold()` 比较、排名（完全相同 2、开头 1、包含 0）、人物 tag 的可读形式、`WORK_NAMES`、作品树（`works.TREE` 的递归，`UNION` 防环）、`skin_zh`、「父 · 皮肤」组合。
  - `str.casefold()` 是逐码位的完全大小写折叠，Rust 的 `to_lowercase` 不同（ß、ﬁ、ſ…）。`tools/gen_casefold.py` 用服务 App 的 Python 生成码位表 `casefold.rs`（Python 3.12.12，Unicode 15.0，1,530 个码位）。
  - 照搬了一处旧代码的细节：作品的名字 / 链接命中那一段是 dict 赋值，同一作品多行命中时按行的顺序后者覆盖前者（`abc` 完全相同、`abcd` 只是包含 → 0 而不是 2）。
  - Python：名字已在核心时 `search_hits` 调核心；旧实现改名 `search_hits_python`，没切换的库照用，也作对照的标准答案。
- **顺带修的实现 bug（N3 引入）**：`cli.rs` 的 `COMMANDS` 没有列 N3 新加的 `names-command`、`cutover-names`、`sync-names`、`verify-names`、`names-project`。桌面 App 里 `ILLUSTASH_TERMS_BIN` 指向桌面外壳，外壳只把表里的子命令当命令跑，其余当成开窗口——App 启动时的名字切换和 NameUpdater 的写入都会走不通。补上，并加测试：`run` 认的每个子命令都必须在表里。
- **测试**（全量：cargo 76 + 3、clippy 零警告、JS 40、Python 767 项通过，跳过 11）：
  - `tests/test_terms_names_contract.py`：
    - `SwitchedNamesContract`：`switch()` 照服务端顺序切换（`adopt_labels` → `adopt_characters` → `adopt_names`），快照走切换后的读路径（`names.view` 给出的投影、核心搜索），N0 的断言原样通过；切换后的全量快照与切换前旧代码的快照逐字相等。
    - 搜索全量对照：夹具里每个名字、别名、链接 key、文字键、皮肤词、词表名，原样、大写、casefold、可读形式、随机切片，加 200 个随机拼的「父 · 皮肤」，外加 `%`、`_`、`\`、`Straße` 等，共 300 多个查询，核心与 `search_hits_python` 逐个相等。
    - `SearchEdges`：切换前补进夹具没有的两种形状（作品两个名字都命中、只在 casefold 下才对上的 `sß`），同样全量对照。
    - 投影外的键退回 `View`、预演连接用 `View`。
  - `tests/test_terms_switched_suites.py`（标签 → 角色 → 名字都切）照跑。
- **变异检查**：
  - 作品命中改成取最大（`SearchEdges` 2 处失败）；
  - 不再计入皮肤词（3 处失败）；
  - casefold 退化成 `to_lowercase`（`SearchEdges` 1 处，cargo 单测 1 处）；
  - `COMMANDS` 漏一个子命令（cargo 1 处）；
  - `ProjectedView.entity` 不分 `skin_only`（2 处）；
  - 投影外的键不退回 `View`（1 处）；
  - 预演连接不走 `View`（1 处）。
  - 第一轮对照没抓到「后者覆盖」和 casefold 两处：夹具里没有这两种数据形状，补了 `SearchEdges` 才抓到。

### N5 历史文案进核心、拆掉剩下的预演（2026-09-29，完成）

- **核心生成历史文案**（`crates/core/src/character_labels.rs`）：
  - `Shown::name` = SQL 函数 `entity_name(id)`。名字已在核心时读 `term_name_display`：这个事务改过名字（`names_version` 变了）就先在事务里重建投影（`legacy_names::refresh_projection`）。名字还没切过来时，用移植的 `View`（`names_view`）在同一事务的数据上现算。
  - `works` = `Store._works`（按 `duplicates.grp` 去重的张数）；`lang_of` = `names.lang_of`。
  - 每条命令按旧代码逐字拼文案：`change` 六种、`answer` 三种、随机抽查、换人、检查完了、身份未定、锚点、改资料（用改完之后的名字）、关联到 Danbooru、策略未设置时的合并（用合并之后的名字：先占位、最后改）。`set_kind` 标成原创角色时固定下来的显示名，也由核心读。命令不再收 `label`。
  - **事务中途只重建投影**，不补名字主数据、不记 `names_projected`。第一版中途调了完整的 `after_write`，拒绝对照抓到一个真 bug：`set_profile` 新建的作品这时还没镜像进 `term_tree`，主数据同步漏了它的名字，而提交时又因为版本号已记下而跳过。
- **写入口去掉预演**：`set_profile`、`link_local`、策略未设置时的合并、`change`、`answer`、`answer_audit`、换人、`sweep`、`uncertain`、`set_kind`、`set_mascot`、`set_work`、`shared_oc`、锚点修正。预演机制整个删掉，`names.view` 的预演例外也去掉。
  - **检查的先后**：构建器只做 Python 才看得到的参数检查，用从旧方法里抽出的辅助函数，文案相同，旧写路径也改用它们。按筛选条件选图只有 Python 能算；旧代码在这一步拒绝时，构建器不当场抛，把文案交给核心（`asked_refused`），核心在旧代码选图的同一位置报出。
  - **新建作品的 id**：旧代码只在真要新建作品时抽一个 uuid，而之前可能有很多种拒绝。构建器留 `work_id: false`；核心走到新建的那一刻以退出码 5 要 id，标准错误给出它在命令里的 JSON 指针，什么都不写。`Store._core` 抽一个 uuid 填进去，再执行一次。uuid 的抽取次数和先后与旧代码相同。
  - 核心补齐了原来靠预演挡在前面的几处：
    - tag 已被别的角色关联时，报错里是那个角色的**名字**（原来是 id）；
    - 名字、别名按 Python `str.strip()` 去空白（多去 `\x1c`–`\x1f`）；
    - `set_work` / 资料的 `work` 按 Python 真值判断（`5`、`true` 这类非字符串的真值 → 「作品不存在」）；
    - `uncertain` 的选图拒绝。
  - 锚点的 post ID：旧代码按 Python `str.isdigit` 查，构建器先查；核心的兜底改成包含它的 `char::is_numeric`，比如 `５`，两边都接受。
- **测试**：
  - `tests/test_terms_character_refusals.py` 新增 11 组：`change`（含多角色、筛选条件、排除列表）、`answer`、`answer_audit`、换人、`sweep`、`uncertain_identity`、锚点、`set_kind` / `set_mascot` / `set_work`、`shared_oc`、`set_profile`（逐项变化加 150 组随机组合）、`link_local`。
  - 夹具补了皮肤、一组复制品、随机抽查、两个自建角色。四种组合都跑：有 / 无身份策略 × 名字在 / 不在核心。
  - 每次调用比较是否拒绝、文案、返回值（含重新渲染的文案）；最后比较原始历史文案、结果状态，并跑 `verify-characters`（名字在核心时加 `verify-names`）。60 项通过。
  - `test_merge` 原来只比返回值（按当前名字重新渲染，看不出写进去的原始文案），补了原始文案的比较。
  - `tests/test_terms_character_performance.py`：16 种写入口执行时，Python 自己的连接上没有任何写语句（不再先演一遍）。
- **变异检查**（都能报出）：
  - `change` 文案改一个字；
  - 张数不按复制品组去重；
  - 核心不读 `asked_refused`；
  - 核心自己生成新作品的 id（不向 Python 要）；
  - 事务中途不重建投影；
  - 合并文案两边名字对调（补比较原始文案之前抓不到）；
  - tag 冲突的报错用 id；
  - Python 真值判断改错。
- **与旧代码仍不同的一处**（非法输入）：`set_work` / 资料的 `work` 传列表或对象时，旧代码 SQLite 绑定参数失败（`ProgrammingError`，HTTP 500）；现在核心报「作品不存在」（400）。前端不会发这种值。
- 全量：cargo 77 + 3、clippy 零警告、JS 40、Python 816 项通过（跳过 11）。

### N6 真实副本验收（2026-09-29，完成，用户授权拷贝）

脚本 `tools/verify_terms_names.py`（2026-10-05 随参考实现删除，#186；见 git 历史）（子命令 `projection`、`sync`、`cutover`、`threeway`、`double`、`undo`、`sabotage`；拒绝对 `~/Pictures/Illustash` 运行）。

**拷贝**：
- 真实图库没有进程占用（`pgrep`、`lsof`）。拷贝前后，三个库、`-wal` / `-shm` 与 `album-char-map.json` 的 mtime 和大小都没变，没有出现新的 `-wal`。
- **与交接不同的情况**：`library.sqlite` 旁边有一个 34 MB、没检查点的 `-wal`（App 上次关闭时没合并回主库）。按原定的 `immutable=1` 备份会忽略它，拿到的是旧数据。所以改为：把主库连同 `-wal`、`-shm` 原样只读拷进 scratchpad，在副本上用备份接口生成一致副本（`quick_check` ok）。`source.sqlite` 用 APFS 克隆。
- **与交接不同的情况**：真实图库的核心 schema 是**第 3 版**，不是第 4 版（第 4 版只在 terms-scaffold 分支上，图库一直用 main 的代码）。
- 副本 A（名字不切，作基线）、B（名字切换）、C / D（从 A / B 的当前状态再拷，作双跑与撤销）。

**结果**（全部零差异）：

| 步骤 | 结果 |
|---|---|
| 升级 | 3 → 5 一次完成（0.02 秒），第 4 版删掉 8 张冻结表（`artist_verdicts`、`legacy_asset_tags`、`legacy_assertions` 等 6 张） |
| N1 投影 | 9,276 个身份（完整名与皮肤短名共 18,552 行）、10,718 个文字键，与旧 `View` 逐字相等；投影没有多出的行 |
| N2 收编 | `names` 40,773、`display_names` 822、`name_aliases` 2,572 行，零差异；再跑不改任何行 |
| 切换 | App 路径（`adopt_names`，含备份）成功。`term_names` 比 `names` 多 80 行，是画师的名字（画师那一步就在核心里，名字收编按设计不碰） |
| 三方对照 | ① 名字：旧 `View`（A）、核心投影（B 的表）、App 的 `ProjectedView` 与 SQL 函数 `entity_name` / `entity_info` / `work_name`（B），全部身份与文字键逐字相等。② 搜索：4,057 个查询，核心搜索（B）与旧 `search_hits`（A，以及 B 上的旧实现）逐个相等。查询由名字、别名、链接 key、文字键随机抽 1,500 个，加切片、大写、可读形式、「父 · 皮肤」组合构成。③ App 读接口 4,300 多项全部相同，包括总览、角色列表、作品、筛图、搜索、详情 3,015 张、资料 430 个、统计、身份队列与关系，以及全部 1,478 条历史的重新渲染。④ `verify-names` 零差异 |
| 双跑 | C（名字不切：Python 直接写名字表，文案由核心用移植的选名规则现算）与 D（名字已切）执行同一串 26 步：NameUpdater 整批替换（人物、作品、皮肤、成功的空结果）、Danbooru 别名（人物、作品，网络用固定假数据）、非法来源（两边都拒绝）；改名、别名、清掉用户名字、`change`、换人、`sweep`、`set_kind` 两次、新建角色、合并。每步之后名字三表、原始历史文案逐一相等，D 的投影等于旧 `View`，`verify-names` 零差异 |
| 连续撤销 | 两边从最新一步起连续撤销 50 步：8 步来自双跑，42 步是真实历史，包括身份关系决议、回答提问、排除、画师确认、换人、设为 / 移出。每一步之后全量核对，同上，全部一致 |
| 故意破坏 | 改坏投影的一个显示名：对照报出 1 处。改坏名字主数据的一行：`verify-names` 报出（多 1 行、少 1 行）。第一次改到的是画师的名字（不在核对范围里，按设计），改为选名字范围内的一行 |

**性能**（副本上，毫秒，中位数）。对照的「A」是同一份新代码、名字未切：NameUpdater 直接写 Python 表，文案由核心用移植的选名规则现算，搜索是旧 `search_hits`。

| 项 | 名字未切（A） | 名字已切（B） |
|---|---:|---:|
| 升级到第 5 版 | — | 20 |
| 名字切换，App 路径 | — | 10,400（其中切换前备份 1.1 GB 约 9,600，`cutover-names` 本身 780） |
| 显示投影整表重建（`names-project`，含进程启动） | 旧 `View` 装载一遍 70（Python 进程内） | 100（18,552 + 10,718 行） |
| 收编 `sync-names` | — | 首次 390，再跑 240 |
| 搜索（4,057 个查询平均） | 64.7 | 34.3 |
| NameUpdater 单次整批替换 | 4 | 195 |
| 改资料（改名） | 53 | 298 |
| `change` 设为 / 移出 | 96 | 64 |
| `sweep` | 159 | 101 |
| 角色搜索 `entities('爱')` | 52 | 43 |
| 角色列表 | 480 | 409 |
| App 读接口全量（4,300 多项） | 91.9 秒 | 88.7 秒 |
| 库体积 | 1,098,862,592 字节 | 1,098,862,592 字节 |

- **变慢的**：写名字的操作。每次都要启动核心、补齐名字主数据、整表重建投影（约 0.1 秒）；改资料时事务中途为了文案再重建一次。NameUpdater 一次后台刷新要写上千个键，每个键约多 0.19 秒。`names-command` 有 `batch`，可以把多条放进一个事务，但 NameUpdater 现在是逐键调用。
- **变快的**：只读名字的写入口（`change`、`sweep`）和读路径。它们读投影，名字没切时核心要现算选名规则（约 30 毫秒）；搜索进核心后快了一半。
- **库体积没变**：名字的新表约 6.5 MB（`term_names` 与其索引 3.6 MB、`term_name_display` 1.8 MB、`term_name_choice` 0.8 MB、文字键两表 0.2 MB），用的是第 4 版删冻结表腾出的空页。
- 按约定只测量，没有优化。待优化：NameUpdater 批量交付（一次刷新一个事务）、投影按受影响的身份增量重建、改资料时中途与提交时的两次重建合成一次。

副本（A–E 与 B 的切换备份）用完已删，只留下 scratchpad 里的 JSON 报告。

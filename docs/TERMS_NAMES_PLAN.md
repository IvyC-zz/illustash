# 名字投影与收编方案（词条第 4 步的名字这一半）

状态：已完成的方案，留作代码注释按节号引用的设计记录，2026-11-12 精简。现行行为、映射、与旧行为的差别和验收见 [TERMS_NAMES.md](TERMS_NAMES.md)，schema 见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 10、14 节。本文只留别处没有、仍然有效的设计和用户决定。方案原来让旧名字表成为核心维护的兼容表，这部分已被词条核心第 8 版（#201，2026-10-20）取代：三张旧表删除，`term_names`、`term_key_names`、`term_key_aliases` 是唯一权威，收编与切换命令删除。

2026-09-29 起草，同日用户确认第 5 节并完成。前置契约是角色那一步的决定 D4（[TERMS_CHARACTERS_PLAN.md](TERMS_CHARACTERS_PLAN.md) 第 14 节）。代码注释里的 N0–N6 指第 4 节的分段，第 6 节记各段定下来、现在仍然成立的设计；逐段的实施流水和测量在 git 历史里。

## 1. 起点

收编之前，名字数据是三张表：按身份的 `names`（`entity_id, lang, value, source, role(display/alias), preferred, position`）、按文字键的 `display_names`（`(kind, key)` = 作品的 qualifier、皮肤的 variant）和别名 `name_aliases`。选名规则（`View` 的 `choose`、`entity`、`base`、`own_name` 与词表）注册成 SQL 函数 `entity_name()`、`entity_info()`、`work_name()`，嵌在 34 处查询里；搜索、批量显示和历史文案也直接用它。`name_cache`、`name_settings` 是名字更新器的联网缓存和设置，不算名字数据。

## 2. 目标

1. **选名规则进核心**：核心按与 `View` 逐条相同的规则算出显示投影，在每个写事务里维护，显示读路径都读投影。
2. **名字主数据进核心**：名字更新器只把结果交给核心，不直接写表。
3. **历史文案由核心生成**，写入口不再先演一遍旧代码。

界面、HTTP API、选名结果、搜索结果、历史文案逐字不变（等价迁移）。

## 3. 设计要点

- **显示投影表**（核心维护，物化）：按身份一行 `term_name_display`（完整显示名、皮肤短名 skin_only、名字来源、zh / ja / en、tag）；按文字键一行 `term_name_choice`（`(work, qualifier)`、`(skin, variant)` 等的 `choose` 结果，供 `work_name(tag)` 和皮肤名用）。
- **失效依赖**：身份的属性（kind、original、parent、variant、merged_into）、当前链接、名字行、文字键名字、词表。凡是改到这些的核心写事务，在同一事务里维护投影；判断依据是 `meta.names_version`（`entities`、`links`、`names`、`display_names` 上的触发器递增它）。最初是整表重建，之后改成按依赖闭包局部维护（#103、#104），见 [TERMS_NAMES.md](TERMS_NAMES.md) 第 3 节。
- **词表**：`SKINS`、`WORK_NAMES`、`readable()`、`skin_zh()` 移植进核心（`names_view.rs`）。
- **写路径**：用户名字经角色核心命令；名字更新器的结果经名字命令交给核心（第 5 节 N2），按来源整批替换，成功的空结果让旧名字失效。

## 4. 分段

| 段 | 内容 |
|---|---|
| N0 夹具 | 在旧代码上钉住选名行为：qualifier 作品读 `display_names`、词表例外、英文回退、合并代表借名、本地 / 原创只用自己的名字、皮肤名与「以前的人物变成的皮肤」、作品的用户名字、`put` / `put_name` / 替换的写入规则、历史文案的重新渲染、搜索 |
| N1 投影 | 核心按 `View` 规则建显示投影（`names_view.rs`，命令 `names-project` 整表重建），与旧 `View` 全量逐字对照 |
| N2 名字收编 | `sync-names`：名字三表收编进核心主数据（已随 #201 删除） |
| N3 核心写名字 | 名字更新器的交付、所有写名字的入口改走核心（`names-command`）；核心命令同事务维护投影 |
| N4 读路径与搜索 | 显示读路径改读投影；名字搜索进核心（`names_search.rs`，命令 `names-search`） |
| N5 历史文案进核心 | 历史文案由核心按投影生成（`character_labels.rs`）；写入口去掉「先演一遍」 |
| N6 真实副本验收 | 结果见 [TERMS_NAMES.md](TERMS_NAMES.md) 第 6 节 |

## 5. 用户确认的决定

2026-09-29 用户确认。

| # | 问题 | 决定 | 理由 |
|---|---|---|---|
| N1 | 搜索这一步是否进核心 | 原建议留在 Python。**用户决定：这一步就搬进核心** | 它只做字符串匹配，没有选名规则；用户希望一并离开 Python |
| N2 | 名字更新器怎么把结果交给核心 | **核心命令**（像角色命令一样，标准输入给 JSON：按来源整批替换、成功的空结果、display / alias / 首选 / 顺序），不走证据包文件 | 名字是显示数据，不参与归属判定；证据包的格式和按指纹去重是为机器证据设计的，名字用不上 |
| N3 | 名字在核心里怎么存 | **按原来的形状收进 `term_names`**（来源、语言、角色、首选、顺序），用户名字就是来源 `user`，不拆成证据和裁决 | 选名规则本来就按来源排优先级；拆成证据 / 裁决不改变任何结果，只增加复杂度 |
| N4 | 历史文案进核心、写入口去掉预演放在这一步吗 | **放在这一步的 N5 做** | 名字规则进了核心之后顺手就能做 |

## 6. 各段的设计

### N1 显示投影

`crates/core/src/names_view.rs` 逐条移植 `View` 和词表，结果写进 schema 第 5 版的 `term_name_display(entity, skin_only, …)`、`term_name_choice(kind, key, …)`。照搬的细节：

- `skin_zh` 按词长排序时，同长的词保持词表原顺序（稳定排序）；
- `readable` 的去 qualifier 正则、Python `str.title()` 的大小写规则；
- `min` 取第一个最小值；wiki 的中文按 bangumi 排，只对中文生效。

投影只收库里出现过的键；别的键按这个键自己的行现算（`names_view::choose_key`，[TERMS_NAMES.md](TERMS_NAMES.md) 第 3 节）。

### N3 名字写入走核心

- **提交钩子**：所有核心写事务的统一提交点 `schema::commit_writing` 调用 `legacy_names::after_write`：`meta.names_version` 与上次投影时记下的 `names_projected` 不同，就在同一事务里维护显示投影。日常命令不碰名字表，只多两次 meta 查询。
- **`names-command`**（`crates/core/src/names_commands.rs`），标准输入一个 JSON `{"op": …}`，一个事务，报错文字与旧代码相同：
  - `replace`：按来源整批替换，空列表就是成功的空结果；
  - `danbooru_aliases`：人物按 tag 找身份写；作品写按文字键的别名，并同步到 qualifier 对应的作品身份；
  - `bangumi_alias`；
  - `seed_index`，以及种子用到的 `put`；
  - `alias`：相册标题用；
  - `batch`：多条放在一个事务里。

### N4 读路径与搜索进核心

- 显示读路径读投影；投影外的键（任意 tag 的 `work_name`、词表里有但库里没有的作品）按键现算。
- **搜索**（`crates/core/src/names_search.rs`，命令 `names-search`，标准输入 `{"query": …}`，输出 `{"hits": {身份: 排名}}`）逐条移植旧 `search_hits`：
  - `LIKE` 预筛（SQL 语句逐字照搬，所以只对 ASCII 不分大小写这一点也相同），再按 Python `str.casefold()` 比较；
  - 排名：完全相同 2、开头 1、包含 0；人物 tag 另比可读形式；`WORK_NAMES`；作品树（`works.TREE` 的递归，`UNION` 防环）；`skin_zh`；「父 · 皮肤」组合；
  - `str.casefold()` 是逐码位的完全大小写折叠，Rust 的 `to_lowercase` 不同（ß、ﬁ、ſ…）：码位表 `casefold.rs` 由 `tools/gen_casefold.py` 生成（Python 3.12.12，Unicode 15.0）；
  - 照搬了一处旧代码的细节：作品的名字 / 链接命中那一段按行的顺序后者覆盖前者（`abc` 完全相同、`abcd` 只是包含 → 0 而不是 2）。
- 核心的子命令都要列在 `native/terms/src/cli.rs` 的 `COMMANDS` 里：桌面外壳兼任核心程序时只把表里的子命令当命令跑，其余当成开窗口。有测试钉住 `run` 认的每个子命令都在表里。

### N5 历史文案进核心、拆掉预演

- **核心生成历史文案**（`crates/core/src/character_labels.rs`）：
  - `Shown::name` = SQL 函数 `entity_name(id)`，读 `term_name_display`；这个事务改过名字（`names_version` 变了）就先在事务里重建投影（`legacy_names::refresh_projection`）。**事务中途只维护投影**，其余留到提交时。
  - `works` = 按复制品组去重的张数；`lang_of`：打字的名字按假名 → ja、汉字 → zh、其余 → en 归语言。
  - 每条命令按旧代码逐字拼文案：`change` 六种、`answer` 三种、随机抽查、换人、检查完了、身份未定、锚点、改资料（用改完之后的名字）、关联到 Danbooru、策略未设置时的合并（用合并之后的名字：先占位、最后改）。`set_kind` 标成原创角色时固定下来的显示名，也由核心读。命令不收 `label`。
- **写入口不再先演一遍旧代码**：调用方只做它才看得到的检查（按筛选条件选图、联网查版权），其余检查由核心按旧顺序、用旧文案做（[TERMS_CHARACTERS_PLAN.md](TERMS_CHARACTERS_PLAN.md) 第 8 节）。新建作品的 id 在核心走到那一步时向调用方要（`Error::NeedId`，退出码 5）。
- 原来靠预演挡在前面、由核心补齐的几处：tag 已被别的角色关联时，报错里是那个角色的**名字**；名字、别名按 Python `str.strip()` 去空白（多去 `\x1c`–`\x1f`）；`set_work` / 资料的 `work` 按 Python 真值判断；`uncertain` 的选图拒绝；锚点的 post ID 用包含 `str.isdigit` 的 `char::is_numeric`（比如 `５` 也接受）。

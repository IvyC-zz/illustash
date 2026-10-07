# 名字投影与收编（词条第 4 步的名字这一半）

2026-09-29，[TERMS.md](TERMS.md) 第 4 节第 4 步里角色那一步留下的名字（[TERMS_CHARACTERS.md](TERMS_CHARACTERS.md) 的 D4）。做法与前几步相同：先投影对照，再收编，然后切写路径和读路径，最后在真实库副本上验收。已完成，在 `terms-scaffold` 分支；回合 main 由用户另行决定。

本文是结果说明。方案、用户确认的决定（第 5 节）和逐段的实施记录（N0–N6）在 [TERMS_NAMES_PLAN.md](TERMS_NAMES_PLAN.md)；schema 在 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 10 节。

**范围**：
- 角色、皮肤、作品的名字（`names`）、按文字键的名字与别名（`display_names`、`name_aliases`）；
- 选名规则（`tests/reference/web/names.py` 的 `View` 与词表）、名字搜索；
- 历史文案的生成，以及剩下写入口的预演（拆脚手架阶段 C 里依赖名字的部分）。

**不在范围**：
- `name_cache`、`name_settings`：NameUpdater 的联网缓存与模型设置，外围，仍是 Python 的表；
- 画师的名字：画师那一步就在核心的 `term_names` 里；
- NameUpdater 的联网查询本身：它仍是 Python 的 worker，只把结果交给核心。

## 1. 映射

原则是**等价迁移**：界面、HTTP API、选名结果、搜索结果、历史文案逐字不变。旧表变成由核心维护的**兼容表**（同名同形，加写保护），核心另存主数据。

| 旧 | 核心主数据 | 切换后 Python 读到的 |
|---|---|---|
| `names`（角色、皮肤、作品的行） | `term_names`，按原样存（来源、语言、display / alias、首选、顺序；用户名字就是来源 `user`，决定 N3） | 兼容表 `names` |
| `display_names` | `term_key_names` | 兼容表 |
| `name_aliases` | `term_key_aliases` | 兼容表 |
| 选名规则 `View`（`choose`、`entity`、`base`、`own_name`）与词表 `SKINS`、`WORK_NAMES`、`readable`、`skin_zh` | 逐条移植（`crates/core/src/names_view.rs`），算成显示投影 `term_name_display`（每个身份的完整名与皮肤短名）、`term_name_choice`（每个文字键） | 显示读路径读投影（第 3 节） |
| `names.search_hits` | 逐条移植（`names_search.rs`），命令 `names-search` | `search_hits` 调核心 |

Python 里的词表副本仍给 NameUpdater 用，两边由测试钉住相同。`str.casefold()` 按码位表移植（`casefold.rs`，由 `tools/gen_casefold.py` 从服务 App 的 Python 生成）。

## 2. 切换

- 命令 `cutover-names`，前提是角色已在核心。一个事务里完成：最后一次收编（必须零差异）、三张名字表加写保护、写 `meta.terms_names=core`、整表建显示投影。
- 服务启动时 `adopt_terms` 按「标签 → 角色 → 名字」顺序切换（`tests/reference/web/server.py` `adopt_names`）。切换前先备份；失败就留在旧表，下次启动再试。
- 已切换的库每次启动先跑一次核心 `migrate`（拆脚手架阶段 A 的 `upgrade_terms`）。真实图库现在是第 3 版，第一次用这个分支启动时会升到第 5 版，同时删掉冻结的旧表。

## 3. 切换之后

**写**：
- NameUpdater 的整批替换、Danbooru / Bangumi 别名、索引种子，以及相册标题别名，都走核心命令 `names-command`。报错文字与旧代码相同，核心拒绝时转成 `ValueError`。
- 用户改名、别名、合并、作品编辑，本来就在角色的核心命令里。
- 所有核心写事务的统一提交点 `schema::commit_writing` 会检查：`names_version` 变了，就在同一事务里补齐名字主数据、整表重建显示投影。
- Python 直接写名字表会被写保护拒绝。

**读**：
- `names.view(c)`：名字已在核心时给 `ProjectedView`，接口与 `View` 相同，按 `names_version` 在连接之间共享快照；否则给 `View`。三个 SQL 函数 `entity_name` / `entity_info` / `work_name` 经它注册，嵌着它们的 34 处查询一行没改。`Store._display_reader`、`names.action_label`、NameUpdater 的统计与翻译前检查也都用它。
- 投影只收库里出现过的键。别的键（任意 tag 的 `work_name`、词表里有但库里没有的作品）由 `View` 选；未装载快照时的单次查询只读一行。
- 搜索：`search_hits` 调 `names-search`，旧实现保留为 `search_hits_python`，没切换的库照用，也作对照的标准答案。

**历史文案与预演**：
- 角色命令的历史文案由核心按旧代码逐字生成（`character_labels.rs`）。名字读显示投影；名字还没切的库，核心用移植的选名规则现算。
- 写入口都不再「先演一遍旧代码」：构建器只做 Python 才看得到的参数检查、按筛选条件选图、联网查版权，其余检查由核心按旧顺序、用旧文案做。
- 新建作品的 id 在核心走到那一步时向 Python 要，uuid 的抽取次数和先后与旧代码相同。

## 4. 回退

只靠切换前的自动备份（`library-backups/`）。切换是单向的：切换后 main 的旧代码写名字表会被写保护拒绝。

## 5. 与旧行为的差别

- **非法输入的一处**：`set_work` 或改资料时 `work` 传列表或对象，旧代码是 SQLite 绑定参数失败（`ProgrammingError`，HTTP 500），现在核心报「作品不存在」（400）。前端不会发这种值。**2026-09-30 用户确认接受。**
- 除此之外，规则、文案、结果都没有差别。拒绝对照、双跑、真实副本上的三方对照都是逐字比较。
- 预演原来挡在前面、核心自己的检查与旧代码不一致的几处，拆掉预演时已在核心补齐：tag 冲突的报错里是角色名字、Python 的 `strip` 语义、`work` 的真值、`uncertain` 选图时的拒绝。

## 6. 验收

**测试**（全量：cargo 77 + 3、clippy 零警告、JS 40、Python 816 项，跳过 11）：
- 契约、投影、收编、写入双跑、拒绝对照（原 `test_terms_names_contract`、`test_terms_names_projection`、`test_terms_names_sync`、`test_terms_names_writes`、`test_terms_character_refusals`）：2026-10-07 起改为 `native/terms/tests/replay.rs` 中同名模块的核心调用重放（#9），比较口径见该文件开头。
- 选名规则：`native/terms/src/app_names.rs` 的 `matching_tests`、`sources_tests` 在原测试保存的图库状态上用同样的提供方回答驱动原生名字任务，核对原期望；传输层的重试与节奏见 `app_name_sources.rs` 的 `network_tests`。
- 每一段都做了变异检查，见方案第 6 节。

**真实副本**（N6，详见方案第 6 节）：
- 投影与旧 `View` 全量零差异：9,276 个身份、10,718 个文字键；收编零差异，再跑不改。
- App 路径切换成功。三方对照零差异：名字全量、4,057 个搜索查询、4,300 多项读接口，含全部 1,478 条历史文案。
- 双跑 26 步、连续撤销 50 步（其中 42 步是真实历史），每一步都零差异。
- 故意破坏投影或主数据，都能报出。
- **真实数据里的新情况**（都不是差异）：
  - 图库的核心 schema 是第 3 版，一次升到第 5 版；
  - `library.sqlite` 带着一个没检查点的 34 MB `-wal`，拷贝方法因此改为连同 `-wal` 一起只读拷贝；
  - `term_names` 里另有 80 行画师的名字，按设计不在名字收编的范围里。

**桌面 App 在真实副本上走一遍**（2026-09-30，合入 main 之后，用户同意）：
- 副本：两个库连同 `-wal` 只读拷出后，在副本上用备份接口生成；`source.sqlite`、缩略图、特征用 APFS 克隆。`library.json` 改名为「名字切换桌面验收副本」。拷贝前后原库的 mtime 与大小不变。
- 启动：main 的桌面外壳（debug 构建，产物放在 scratchpad，不碰主目录的 `desktop/target`）。参数 `--library <副本> --no-sync --no-names --no-similar --no-index-check`；`ILLUSTASH_APP_SUPPORT` 指到 scratchpad，事后核对全局的 `libraries.json` 没被修改。
- 启动时自动完成：核心 schema 3 → 5（删冻结表），名字切换（含备份）13 秒；`names_projected` 等于 `names_version`。
- 界面：图库首页正常（角色显示中文名），控制台没有错误。
- 经同一个服务的接口：
  - 搜索「圣园」得到圣园未花；
  - 把她改名为「验收改名」：历史文案「已更新 验收改名 的资料」，按新名字、旧名字都能搜到；
  - 撤销后名字回到「圣园未花」。再改名、撤销两轮，结果相同。
- 关闭 App 前 `verify-names`、`verify-characters` 都零差异。副本、备份与构建产物用完已删。
- **记下的性能**：
  - App 里改一次名约 1 秒（验收脚本测的是 0.3 秒，差在服务端接口与名字快照重载）；
  - **撤销一次改资料约 9.4 秒**：撤销资料步骤总是全量重建身份图、重算证据与归属。旧代码也是这样（`_profile_changed(c, True)`），不是新差异。后来已优化（撤销改名 / 改链接在合成数据上约 40 / 114 ms，见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md)）。

## 7. 体积与性能

见方案第 6 节 N6 的表：
- **库体积**：没变。新表约 6.5 MB，用的是删冻结表腾出的空页。
- **搜索**：64.7 → 34.3 毫秒。
- **只读名字的写入口和读路径**：稍快。
- **写名字的操作变慢**：NameUpdater 单次整批替换 4 → 195 毫秒，改资料 53 → 298 毫秒。每次都要启动核心、补齐主数据、整表重建投影；改资料时，事务中途为了文案还要多重建一次。
- **名字切换**：App 路径 10.4 秒，其中 9.6 秒是切换前的备份。

## 8. 没有做的

- **性能优化**：只做了测量。待优化：NameUpdater 批量交付、投影增量重建、改资料时的两次重建合成一次。
- **拆脚手架阶段 C 的其余部分**：新图库直接以核心形态创建；删旧 Python 写路径与切换前的收编、对照命令；兼容表去掉之前，读路径要全部改读核心。

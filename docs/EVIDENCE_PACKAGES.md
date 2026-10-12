# 证据包 v1（`illustash-evidence/1`）

2026-09-28。worker（识别、溯源、分组、规则）把机器结果交给词条核心的唯一格式，见 [TERMS.md](TERMS.md) 第 4 节第 1 步。
它合并了两份已有契约：[RESULT_PACKAGES.md](RESULT_PACKAGES.md) 的离线识别结果包（目录、清单、文件摘要、导入去重）和未合并分支 `codex/research-boundary` 的研究 worker 契约（`docs/RESEARCH_BOUNDARY.md`：资产命名空间、任务终态、证据带来源和方法、分数带量纲）。

解析与校验在 `crates/core/src/package.rs`，导入在 `crates/core/src/store.rs` 的 `import_package`；命令行校验：

```sh
cargo run --manifest-path native/terms/Cargo.toml --bin illustash-terms -- validate native/terms/tests/fixtures/evidence
illustash-terms import --db <library.sqlite> --library-id <id> --package <目录>
```

退出码 0 表示合法，2 表示不合法或读取失败；标准输出只有一行 JSON 摘要，错误写标准错误。校验和导入都不联网、不加载包里的代码或模型。校验通过不代表授权执行、联网或上传。

## 1. 边界

- 包里只有**机器证据**：图、词条 key、来源、版本、分数、证据 JSON。人工确认、否定、撤销不在包里：来源 `user`、`human`、`album`、`legacy`、`verdict` 保留，包里出现就整包拒绝；证据行也没有能表达裁决的字段。
- 证据只增不删。导入不会删改已有证据，也不会覆盖人的裁决；否定过的图—词条，新证据进来后仍是「已否定」。
- 研究输出始终是证据。一条证据最终显示为「机器认定」还是「也可能是」，由核心按词条类型的判定器决定，不由 worker 决定。

## 2. 目录

一个包是一个目录：`manifest.json`、`evidence.jsonl`、可选的 `terms.jsonl`，以及清单声明的附件。不接收压缩包；只读取清单声明的文件，不跟随符号链接，未声明的文件忽略。

### manifest.json

```json
{
  "format": "illustash-evidence/1",
  "library_id": "目标 library.json 中的 id",
  "producer": {"name": "provenance", "version": "0.4"},
  "created_at": "2026-09-28T10:00:00+08:00",
  "models": {"wd": {"version": "swinv2_448", "sha256": "模型文件 SHA256"}},
  "assets": [
    {"asset": {"namespace": "photos", "id": "资产 ID"}, "task_id": "t-1", "status": "completed", "error": null},
    {"asset": {"namespace": "photos", "id": "资产 ID"}, "status": "failed", "error": {"code": "network_refused", "message": "HTTP 403"}}
  ],
  "files": {
    "evidence.jsonl": {"bytes": 123, "sha256": "完整文件 SHA256"},
    "wd.npy": {"bytes": 123, "sha256": "完整文件 SHA256"}
  }
}
```

- 未知字段、重复键、非有限数值一律拒绝（整份 JSON，包括 `data` 内部）。
- `producer.name` 是小写标识；`producer.version` 是实现与配置的版本。`models` 可选，记录模型指纹（沿用 RESULT_PACKAGES 的 `sha256`、`tags_sha256` 等），核心只存档不解释。
- `assets`：本包处理过的每张图及其终态，最多 100,000 张，`(namespace, id)` 不重复；资产 ID 区分大小写、原样比较。`task_id` 可选，标识一次尝试。
  - `completed`：`error` 为 `null`。完成而没有证据行表示「查无结果」。
  - `failed` / `cancelled`：必须带 `error:{code, message}`，不能发布证据。错误和取消不能记成查无结果。
  - 只含词条关系证据的包（例如 Danbooru 的层级关系）`assets` 为空数组。
- `files` 必须声明 `evidence.jsonl`；每个文件不超过 1 GiB，大小和 SHA-256 必须与实际一致。附件（例如 RESULT_PACKAGES 的 `wd.npy`、`ccip.npy`）只核对不解释，由对应的外围导入器处理（特征仍进 `pi.db` 一类的外围库，不进词条核心）。文件名只能是字母、数字和 `_ . -`，不以点开头。
- `created_at` 是带时区的 ISO 8601。

**包指纹**：去掉 `created_at` 后的清单规范化 JSON（键排序、无空白）的 SHA-256。清单里有全部文件的 SHA-256，所以指纹覆盖全部内容。

### evidence.jsonl

每行一个 JSON 对象，UTF-8，不允许空行，每行不超过 1 MiB。两种目标二选一：

```json
{"id":"ev-1","asset":{"namespace":"photos","id":"资产 ID"},
 "term":{"kind":"artist","link":{"system":"danbooru","key":"some_artist","url":"https://…"},"name":"Some Artist"},
 "source":"provenance","method":"pixel_hash","version":"pixel_hash/1",
 "score":{"value":0,"metric":"hamming_distance"},
 "observed_at":"2026-09-28T02:00:00Z","data":{"site":"danbooru","post_id":"123"}}
```

```json
{"id":"rel-1","relation":{"kind":"contains","a":{"kind":"character","link":{"system":"danbooru","key":"mika_(blue_archive)"}},
                                           "b":{"kind":"skin","link":{"system":"danbooru","key":"mika_(swimsuit)_(blue_archive)"}}},
 "source":"danbooru","method":"implication","version":"2026-09","score":null,
 "observed_at":"2026-09-28T02:00:00Z","data":{}}
```

| 字段 | 规则 |
|---|---|
| `id` | 本包内唯一，只用于报错定位 |
| `asset` + `term` | 图—词条证据。图必须在 `manifest.assets` 里且为 `completed` |
| `relation` | 词条—词条证据：`kind` 为 `contains`（a 包含 b）、`same_as`（b 是代表）、`different_from`、`excludes`；两端不能是同一个词条。共现由核心算，不走包 |
| `term` | `{"kind", "key"}` 引用核心已有词条（key 不存在则拒绝）；或 `{"kind", "link":{"system","key","url"?}, "name"?}` 按外部链接引用 |
| `source` / `method` | 小写标识。`source` 是产出者一类（`wd`、`ccip`、`provenance`、`duplicates`……），`method` 是具体方法（`tagger`、`pixel_hash`、`gallery_match`、`ocr`……） |
| `version` | 算法、模型或阈值变了就要变。同一目标、来源、方法、版本只能有一条证据 |
| `score` | `null`，或 `{"value": 有限数值, "metric": 量纲}`。距离和概率各写各的量纲，不统一伪装成概率 |
| `observed_at` | 采集时间，带时区的 ISO 8601 |
| `data` | 来源特定的 JSON 对象，可自由扩展，规范化后不超过 64 KiB |

同一个包里，同一目标（对称关系不分端点顺序）、来源、方法、版本出现两次就拒绝。

**按链接引用词条**：链接已属于某个词条就用它，**不管包里写的 `kind`**（人把角色改成「只当标签」后，WD 的证据照样挂在原词条上）。找不到时，只有类型策略允许的类型才新建词条（带这个链接和 `name`）：用户自建的概念（`concept`）和 CP（`pair`）不能由包新建。

**产出者自己的链接系统**：链接的 `system` 与 `producer.name` 相同（例如溯源的 `provenance:<画师 id>`）时，它是产出者给这个词条的身份；按它新建词条时，词条 key 就用这个链接的 key（未被占用时），界面上的 ID 因此不变。

**命名空间 `library`**：键是本图库 `assets.uuid`。核心还没登记、但同库 `assets` 里有的图，导入时当场登记。导入加 `--new-assets` 时，连 `assets` 里还没有的也先登记：同步导入新图时 WD 证据先于 Python 写 `assets` 交来（[TERMS_LABELS.md](TERMS_LABELS.md)）。

`kind` 取值见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 2 节。

### terms.jsonl（可选）

产出者对词条本身的描述，每行一个词条，在证据之前处理：

```json
{"term":{"kind":"artist","link":{"system":"provenance","key":"画师 id"}},
 "links":[{"system":"x","key":"foo_bar","url":"https://x.com/foo_bar"},{"system":"danbooru","key":"foo_(bar)"}],
 "names":[{"value":"フー","source":"pixiv"}]}
```

- 同一个词条只能描述一次；`links[].system` 是小写标识，`url` 可省；`names[].lang` 为空、`zh`、`ja`、`en`，`source` 省略时是产出者。
- 链接：没有就加，url 变了就改；已属于**同一产出者建的**另一个词条（`package:<产出者>` 或 `legacy:<产出者>`）就移过来；属于别处（用户、其他产出者）建的词条则冲突，整包回滚。
- 名字：同一 (词条, 来源, 语言) 的显示名跟随最新声明，旧的被替换。

## 3. 导入

`import_package(连接, 包, 图库 id)` 在 library.sqlite 的一个 `BEGIN IMMEDIATE` 事务里完成，任何一行出错整包回滚（包括本包新建的词条和链接）：

1. `library_id` 必须是目标图库；`assets` 里每张图必须已在核心的 `term_assets` 登记且未移除。
2. 包指纹已在账本 `term_packages` 里：什么都不写，返回 `already`。
3. 逐行解析词条引用，计算**证据指纹**：解析后的目标（资产命名空间与 ID、词条 key）、来源、方法、版本、分数、`data` 的规范化 JSON 的 SHA-256；`observed_at` 不参与。
4. 同一目标、来源、方法、版本已有证据：指纹相同就去重跳过；不同就拒绝整包（「算法或模型变了请升版本号」）。不静默覆盖。
5. 类型策略规定了入库分数下限的（题材：WD 概率 < 0.2），低于下限的行不入库、计入 `skipped`，也不为它新建词条。WD 原始分数（约 2000 万条）留在外围库。
6. 记账本，重算受影响图的投影，提交。

包与证据都不进撤销历史：撤销只针对人的操作。

## 4. 与两份旧契约的对应

| 旧契约 | 在 v1 里 |
|---|---|
| RESULT_PACKAGES：`manifest.assets`（ID 列表） | `assets[].asset`，加命名空间和终态 |
| RESULT_PACKAGES：`labels.jsonl` 的 `{asset, dim, value, confidence}` | 一行证据：`term.kind` 由 dim 决定（general → `topic`，character → `character`，rating → `rating`），`term.link = {danbooru, value}`，`source:"wd"`，`version` 为模型版本，`score:{value: confidence, metric:"wd_probability"}` |
| RESULT_PACKAGES：`models`、`files` 摘要、1 GiB 与 100,000 张上限、禁止 NaN | 原样保留 |
| RESULT_PACKAGES：`wd.npy`、`ccip.npy` | `files` 里的附件，只核对；由外围导入器写 `pi.db` |
| RESULT_PACKAGES：「已有导入资产出现不同内容时拒绝」 | 同一目标、来源、方法、版本内容不同时拒绝 |
| research-boundary：`version:1`、`task_id`、`asset:{namespace,id}` | `format` 串、`assets[].task_id`、`assets[].asset` |
| research-boundary：`worker:{name,version}` | `producer` |
| research-boundary：`status` 与 `error`，失败/取消不发布候选 | `assets[].status` 与 `error`，失败/取消的图不能有证据行 |
| research-boundary：`evidence[]` + `candidates[]`（候选引用证据 id） | 合成一行一条证据：候选的 `subject:{system,key}` 成为 `term.link`，引用的证据内容并入 `data`，`evidence.method` 成为 `method` |
| research-boundary：`score:null` 或 `{value, metric}` | 原样保留 |
| research-boundary：`decision`（confirm/reject/undo） | 不属于证据包。人工裁决只经核心的命令写 `term_verdicts`，撤销走 `actions` |
| research-boundary：`request`（任务、输入路径、联网政策） | 不属于证据包。任务派发仍由核心的任务调度负责，本步不定 |

合并后 `codex/research-boundary` 不再单独维护：Python 溯源进程在第 2 步改为产出本格式；`RESULT_PACKAGES` 的现有导入器在第 3 步改走本格式前保持不变。

## 原生溯源计算交付（2026-10-01）

Rust 用 `illustash-provenance/1` 调用显式配置的计算程序，参数为 request.json、response.json；请求含固定包实例、任务身份、owner_pid、暂存缓存、步骤、资产/画师范围及本次授权。回复须原样返回 format/identity/step，只交机器缓存或哈希。身份/范围不符、超限、符号链接或无效证据均拒绝；计算程序不得直接写正式库。适配器看门狗在父进程异常退出后结束进程组；这不是不可信代码沙箱。

Rust 验证暂存结果，生成与 `tests/reference/provenance/export.py（#9 已删除）` 同格式的完整证据快照，经任务接收门验证/去重后，在机器缓存事务内写机器结果、运行终态和 `native_delivery` 的 sealed 包路径，再调用既有词条核心导入。应用库提交而缓存确认前崩溃，下次打开按内容去重重放；未经接收门的暂存目录不恢复导入。交付按账本顺序进行：数据库忙等暂时失败保留 pending，开库与每 60 秒重试；核心因内容拒绝的包（其中的图已移除、不属于本库、文件损坏）或包目录已不存在，记为 `stale` 并在 `native_delivery.error` 留下原因，不再重试，也不挡住之后的包。溯源包是整份缓存的快照、导入时替换上一份快照，所以本轮最后的结果是 `stale` 时，按当前缓存重新封一个 `resealed-*` 包立即交付，它只列仍在图库里的图；重新封的包若仍被拒绝也记为 `stale`，留待下次研究运行（2026-10-11，#28）。核心对外库和已移除图的拒绝不放宽。人工裁决/历史/投影由核心维护，重跑不覆盖「不是 TA」；并发账号归档新加的 leads 不被旧暂存快照删除。

缓存写入与目录回收（2026-10-12，#31）：接收时机器缓存的十张表与暂存缓存逐行比较（rowid 与带类型的取值），只删除、插入不同的行，各表的行与先后和整表替换相同，rowid 与暂存缓存一致；没有变化的表不写。`.provenance/<任务>` 目录：pending 的包保留到交付成功；已交付的只留最新一个（核心当前持有的快照，供查看），更早的已交付包与 stale 包在交付时删除，stale 原因留在 `native_delivery.error`；没有交付记录的目录（接收门之前失败、停止或进程退出的运行，按上文不恢复导入）保留到下一次开库或运行开始时删除。删掉的包不再需要：交付只读 pending 包，最新结果为 stale 时按当前缓存重新封包。合成 5 万张、三次中位数：接收写入由 441,000 行降到无变化 0 行、一行变化 4 行；一个包约 23 MB，以前每次成功运行各留一个，现在只留最新一个和未交付的。

验收使用合成包、本机模拟站点及计算替身，覆盖导出字节、正式证据去重/人工历史、四类任务、失败重试、取消、超时、崩溃恢复、切库及进程树。真实账号/图库、Windows 实机及互联网吞吐未验证，流量上限不代表响应体内存峰值验收。

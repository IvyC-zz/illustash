# 画师词条（词条第 2 步）

状态：现行参考，2026-11-12。[TERMS.md](TERMS.md) 第 4 节第 2 步：画师在词条核心里的表示、人工裁决和溯源证据包的交接。现有图库 2026-09-28 起从溯源库收编画师、切到核心；收编与切换命令（`sync-artists`、`cutover-artists`，`legacy_artists.rs`）已随 #201 第 4 步删除（2026-10-20），核心第 8 版拒绝没切换完的图库，新建图库由 `new_library` 一开始就在核心里（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 14 节）。

核心持有画师词条和人工裁决；溯源 worker 只写 `provenance.sqlite` 的机器缓存，由 App 封成证据包交给核心（[EVIDENCE_PACKAGES.md](EVIDENCE_PACKAGES.md)「原生溯源计算交付」）。

## 1. 映射

证据包由 `native/terms/src/app_provenance_cache.rs` 按溯源库生成，映射与当初收编相同：

| 溯源库 | 核心 |
|---|---|
| `artists` | `terms`（kind=artist，key 沿用画师 UUID，`#/?artist=<id>` 不变），加一条 `provenance:<id>` 链接 |
| `artist_links` | `term_links`，经 `terms.jsonl` 交来（移到另一位画师就跟着移；挂在别的类型词条上则拒绝） |
| `artist_names` | `term_names` 显示名，每个 (画师, 来源) 一个 |
| `merged_into` | `same_as` 关系证据（方法 `merge`） |
| `image_artists` | 证据：来源 `provenance`，方法照旧；量纲 `exact_match`（像素哈希）、`gallery_similarity`（本地比对）、`ocr_confidence`（OCR）；`status` 记在证据 JSON。溯源库没有算法版本，版本用内容摘要 `v4/<摘要>`：行被改写后另起一条，旧的留着 |
| 图 | `term_assets`，命名空间 `library`（键是 `assets.uuid`，不分 Photos 与文件夹）；不在 `assets` 里的记为已移除，证据和裁决照留、不投影 |
| `image_sources`（原帖） | 不进核心，仍只在溯源库；读取时只读挂上 |

包里用 `provenance:<画师 id>` 链接引用画师，`terms.jsonl` 带各画师的外部链接和昵称；新画师按这条链接新建时 key 就是画师 id，界面链接不变。证据 `data` 的规范化 JSON 按 serde_json（ryu）排版浮点数（`2.1e-9`、`0.000057`），内容摘要因此与收编时一致，否则每条归属会被再加一遍。

## 2. 人工裁决与读取

- 「是 TA / 不是 / 清除」：`POST /api/artist/verdict`（`native/terms/src/app_commands.rs`）调用核心 `store::judge`，写 `term_verdicts` 和一步 `actions`，历史文案不变。「清除」是人收回，不是否定。
- 有效裁决按 `action_id`（历史顺序）取最新，同一步内再按插入顺序。
- 撤销：核心写的步骤，以及收编前留下的 `artist_verdict` 旧步骤（收编时已挂在同一个 action 上），都由核心撤（归属规则见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 4 节）。
- 详情里的画师、「库里 N 张」、按画师筛选、抽查池读核心投影（`native/terms/src/http_artists.rs`）：机器认定或人工确认、且未移除的图可见；抽查池是有强证据的图。显示名按昵称 → X 账号 → Danbooru tag 选。原帖读溯源库，「不是」的判断套用核心投影。
- 人对只有 OCR 线索的图说「是」，显示为已确认并计入「库里 N 张」；「不是」的图在抽查页带标记显示。
- 投影还不按 `same_as` 归并同一位画师。
- `artist_verdicts` 已不存在：已有图库在词条 schema 4 升级时删除，新建图库由 `new_library` 删掉（[TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13 节）。`image_artists` 仍是溯源 worker 的机器缓存。

## 3. 回退

先停止服务并保留当前包与 WAL，再用升级前 `backups/` 的一致性备份恢复相容版本；不能靠删除 `terms_artists` 标记退回旧实现。

2026-09-28 在约 4.8 万张图的一致性副本上切换：4,011 位画师、9,420 条证据、51 条裁决，耗时 0.7 秒；切换前读路径、核心表与切换后读路径逐项对照（显示与确认、库里 N 张、筛选、抽查池、链接、昵称）0 差异；切换后第一个证据包 9,420 条全部判为已有。

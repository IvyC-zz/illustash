# 图库：多图库与文件夹来源

状态：现行参考，2026-11-18。图库来源与包结构；安装资源及平台限制见 [BACKEND_TASKS.md](BACKEND_TASKS.md)。

## 首次建库与参考库

安装包不带 `reference.sqlite` 或个人图库数据；基础角色索引随识别模型下载（见下文[基础角色索引](#基础角色索引)）。桌面 App 第一次启动、没有能打开的图库时显示欢迎页，在图形界面里新建图库（见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md) 的外壳启动段落）。命令行首次使用可执行 `bin/illustash-terms library-create --dest <新包.pilib> --folder <图片目录>`（Windows 可执行文件为 `.exe`），再用 `bin/illustash-terms http --library <新包.pilib>` 打开；服务启动仍需已初始化的图库，不会隐式创建。CLI 不给 `--reference` 时直接使用程序内置的空 source schema，角色、别名、模型运行、图片、标签、决策和 embeddings 均为空；建库不扫描图片，也不启动计算 worker。

设置页新建图库默认查看资源根下的 `review/reference.sqlite`（设置 `ILLUSTASH_SHARED` 时改看该目录）。仅路径不存在时使用内置空 schema；已有但损坏、不完整、不可读的参考库和断链不会静默回退。显式 CLI `--reference` 必须指向有效文件。参考库只读复制表结构与索引，只复制 `characters`、`character_aliases`、`runs` 的行，不复制图片或人工决策；它代表导出时的角色索引，不自动跟随原图库更新。

新包发布 `library.json` 前校验盘点、读取、导入所需列，包括 `assets.hidden/stable_hash/orig_size`、`embeddings.uuid/model/vec`、`labels.source/model_ver/run_id` 等标签导入字段和角色索引字段；照片来源另校验 `assets.z_pk`。保留原有 `source_id/preset` 和文件盘点表的创建兼容，其余不完整参考库拒绝发布并清理本次新包，不改参考输入。处理办法是省略 CLI 的 `--reference` 使用空 schema，或提供符合当前结构的参考库；已有图库不由此入口升级。

2026-10-01 在 macOS arm64 上用白名单基础产物和隔离用户目录实测：无共享资源或参考库时，CLI 首库、HTTP 新建与打开、首次同步以及图片内容变更后的同步均通过；只用合成图片。此结果不代表 Windows 干净机或桌面安装验收（仍由 #71 / #90 承接）。

基础版首次文件夹同步完成盘点和应用导入后可浏览、人工整理；没有可选识别 worker 时状态为「待识别」，不表示盘点失败。回退时使用同版本程序和资源，旧版仍可能要求外部参考库；不要为回退把个人数据放进安装资源。

## 基础角色索引

2026-11-17 用户定（#607）：DeepGHS Character Index 固定版本算出的 7,367 个角色的锚点和阈值，做成识别模型清单里的可选文件 `character-index/character-index-v1.bin`（来源与许可见 [第三方组件](THIRD_PARTY.md#模型与数据)）。它和 WD、CCIP 一起默认下载；「设置 › 数据 › 识别模型」的下载、修复和从文件导入都包括它。已有图库升级后缺这个文件时，按缺失的可选文件提示下载，识别照常进行。

- **接入**：包不另开识别路径。索引任务把核对过大小和 SHA-256 的包复制进任务目录，构建时把包里的角色当作精选条目，与 `char-index/` 里的条目一起算出共享锚点 `char-anchors.*`（同名时 `char-index/` 优先），角色表、别名和索引候选照常写进图库，候选来源为 `char_index`。精选条目按 p90 认领，最近 9 项。
- **启用**：文件就位后，宿主在恢复检查里为当前图库自动启用（首次准备时它和首次同步同时发生，同步排在启用后面，写完自己开始，识别用的就是新锚点）；也可以在「设置 › 数据 › 识别模型」里手动启用（`POST /api/index/base`，状态在 `GET /api/index/base`；`/api/index` 的回复不变）。图库的角色表里已有包里的每个角色时，视为同一版本（已有图库当年装载时个别角色没算出阈值，这类角色照旧不认领）：只装上共享锚点，不重写证据，不改主归属（记为 `adopted`）。其他情况按包重建角色表和索引候选（记为 `rebuilt`）；版本不同的就是一次来源替换，人工结论照旧优先。图库应用库的 `meta.character_index_base` 记着包的 SHA-256 和启用方式。
- **撤回**：按启用时的方式还原：`rebuilt` 的图库按不带包重建，`adopted` 的图库内容本来没动，只换回不带包的共享锚点。人工结论和历史不变。撤回写进共享目录的 `char-index-base.json`，对这台电脑上的所有图库生效：其他图库打开时跟着撤回，之后不再自动启用，要手动再启用。
- **之后的索引任务**：「检查新角色」等索引任务按 `char-index-base.json` 带上或不带包，重建结果和启用状态一致。启用了却找不到完好的包时，任务停下并提示到「识别模型」里修复，不会按不带包的结果改写图库。
- **身份 v2 的参考图清洗**（2026-11-18 评估，#607）：没有采用。清洗要从上游约 18 GB 的 `images.zip` 恢复每张参考图的来源；采用后锚点改变，已有同版本索引的图库要按新版本做来源替换；生产的中心加分位阈值公式对少数共用参考不敏感，也没有它能提高认领的评测结果。需要时另开研究 Issue，用 #557 的评测集比较。

合成测试输入由 `tests/synthetic_source.py` 的 `source_fixture(root)` 与 `synthetic_reference(root)` 构造，只使用显式临时目录和 SQLite/JSON，不调用原生候选建库。前者保留固定小源库及相册映射，后者保留完整参考 schema 与 `characters`、`character_aliases`、`runs` 行；历史升级 schema/producer 和业务期望仍由各自既有测试持有；各测试直接导入 `source_fixture`。

## 出处类型接口

2026-10-02 的后端出处接口在 `crates/core/src/origin.rs`：`Origin` 的静态注册表提供 Pixiv、X、其他的固定 ID、显示名、Photos App 映射，以及可选的文件名/原帖链接解析和外发图片许可。存储位置（文件夹、Photos）与出处分层；账号和凭据属于服务集成，不属于出处实现。新增出处应增加实现和注册项，校验、默认映射、显示与专门原帖统计由注册表读取；当前不加载插件。

未知 ID 在运行期按「其他」处理，显式的新建/修改预设仍只接受已注册 ID；不会把回退结果写回已有数据。「其他」只执行通用本机处理及已授权的哈希/文字查询，没有专门解析或图片外发许可。Pixiv/X 的许可仍须同时满足用户授权和既有小缩略图限制，声明许可不会启动网络请求；完整底线见 [PROVENANCE.md](PROVENANCE.md) 第 1 节。

原帖解析沿用 Pixiv 作品/旧链接/图片 URL 和 X/Twitter 帖子链接的既有口径，正则按需初始化后复用；Pixiv 文件名解析只提供可选入口，X 媒体文件名不推断成推文 ID。溯源只记录 `image_sources`，不改用户指定的 `assets.preset`。Photos 新建默认配置与未配置 `apps` 的扫描使用注册表默认映射；显式映射保持优先，未映射的 App 不自动收录。显式旧包升级沿用历史口径，缺少 `apps` 时按「其他」回填，不套用扫描默认映射。包 schema、`preset_origin` 与开库只读边界保持不变。

现有图库信息在原字段之后新增 `origins: [{id, label}]`，设置的 `package` 与图库管理的 `info` 信息均随之返回。`web/static` 尚未使用该字段，前端接入由 #107 承接。回退到旧后端会省略新增字段，不需要数据库降级或改写已有出处值。

## 来源管理（#107）

2026-10-02 用户决定、2026-10-03 起的行为：

- **出处由用户指定，不按文件夹名猜。** 每个文件夹来源都带出处（Pixiv / X / 其他），`preset_origin` 为 `user`。CLI `library-create` 不给 `--preset` 时为「其他」（`preset_origin: auto`，表示用户没有选）。已有图库里旧的 `auto` 取值原样保留。
- **建库可以带多个来源**：`POST /api/libraries/new` 接受 `sources: [{kind: "folder", path, preset} | {kind: "photos"}]`，来源 id 按顺序为 `s1`、`s2`……；照片图库最多一个，只在 macOS 上可用。旧的 `kind` / `folder` / `preset` 单来源写法照常可用。
- **文件夹不能嵌套**：每个文件夹连同子文件夹一起读取；新文件夹和已有文件夹相同、在其内部或包含它，都直接拒绝并提示冲突的文件夹。按规范化后的真实路径比较，macOS 和 Windows 忽略大小写（`app_paths::overlaps`）。
- **往当前图库添加来源**：`POST /api/libraries/sources/add {kind, path | paths, preset}`；`paths` 一次添加多个同出处的文件夹，全部加完再开始一次同步（中途有文件夹被拒绝时，前面已加入的照常同步，报错指出被拒绝的那个）。
- **设置页界面**：设置「图库」页（`#/settings/library`）里当前图库的来源按出处分组（Pixiv / X / 其他，macOS 另有 iCloud 照片），每组可以「添加文件夹…」（原生窗口多选），每个文件夹可以改出处、指定搬家后的新位置、认回原图或移除；移除前弹窗写明会移除的图片数。「新建图库」按出处分三栏添加文件夹，macOS 可勾选同时读取 iCloud 照片。没有原生选择窗口的环境（非 macOS 的 CLI 网页模式）改为输入路径。
- **移除来源**：`POST /api/libraries/sources/remove {source}` 先只返回会移除的图片数（`{confirm: true, count}`），界面据此显示页面内确认框并等待明确确认，取消、关闭或 Esc 不提交；不依赖桌面插件替换后的 `window.confirm`，也不增加图库页面的 Tauri 权限。重复点击同一来源只保留一个确认流程；确认期间导航或会话改变会使确认失效，提交仍绑定预览时的会话 token。带 `confirm: 1` 才执行。执行时先把这个来源的图在源库里标为已删除（与文件消失相同），再把来源从 `sources` 挪到 `library.json` 的 `removed_sources`（带 `removed_at`），然后开始一次同步，把删除带进浏览用的应用库。人工结论不物理删除。图库至少保留一个来源。
- **重新添加同一个文件夹**会沿用原来的来源 id，同步时按 `files` 表里的记录把这些图恢复（资产 ID 和人工结论不变）；可以换一个出处，这个来源的图随之改出处。
- 整个来源搬家时，点原来源的「这个文件夹搬到了…」选择新位置。来源 ID 和出处保留。同步会重新读取新位置的每个文件，先认回相对路径和 MD5 都相同的图片，再按 MD5 认回剩余的一对一匹配。旧盘可以离线。同一路径换了内容，或相同内容有多个候选且无法区分时，新文件获得独立 ID，旧整理记录保留在原 ID 上。
- 通过「添加文件夹…」加入新来源时，先给新图片独立 ID。点「认回原图…」可查看旧位置和新位置。新来源的文件仍在、同一 MD5 在其他来源的旧位置都已缺失时，再按相对路径和内容的一对一规则列出候选。确认后同步会再核对内容。两份原件都在时，各自保留 ID。新副本上已经做过的整理仍保存在副本自己的 ID 下，撤销认回后可以继续使用。
- 来源设置显示最近一次认回的张数。点「撤销」恢复原位置和原 ID，显式搬家还会恢复来源路径；旧盘离线也可以撤销。若后来同步或其他来源操作已改变所涉及的文件记录，撤销会拒绝整次变更。文件夹里的原图和按 ID 保存的人工结论始终保留。
- 同步或角色索引更新进行中时拒绝修改来源；测试图库（冻结快照）不能修改来源。
- 回退：旧版本不认识 `removed_sources`，会忽略它，已移除来源的图仍是已删除状态；旧版本仍可打开和同步其余来源。

图片详情的「文件形态」显示原图或本地副本；文件夹与 Photos 共用「来源同步」入口。

## 多图库与来源约定

- 图库包由 Rust 应用宿主建立、打开、升级和备份（`native/terms/src/app_library.rs`、`app_create.rs`、`app_service.rs`），最近图库登记在 `app_libraries.rs`；原生宿主用 `--library <图库包>` 指定图库，默认新建位置是 `~/Pictures/Illustash/`，重名自动加序号。
- 源库有 `asset_links`、`assets.source_id`、`assets.preset`（Photos 可读时按 App 回填预设；同步新图时一并登记）。
- **图库切换放在设置的「图库」页**（用户 2026-09-27 定：切换不频繁，不放侧栏），切换 = 宿主停掉旧图库的任务、换到另一个图库；侧栏只在当前是测试图库时在 Logo 下显示一行小字。
- 文件夹扫描（`native/terms/src/app_folder.rs`，文件遍历在 `crates/platform/src/platform_folder.rs`）：只对大小或修改时间变了的文件算 MD5；按 MD5 跟上移动和改名，资产 ID 和人工结论不变；消失的文件标记已删除，同一路径的文件变了就重新识别：旧的标签、向量和识别记录（`recognition_assets`）一起清掉，新结果整份替换。识别记录不清的话，导入会拿新结果和旧记录的指纹比，换了内容或换了执行后端都对不上，整批被拒。`files` 表记录每个文件上次读到的样子；每次同步后新路径一并写进应用库。新建命令和参考库行为见上文「首次建库与参考库」；出处不按文件夹名猜（见上文「来源管理」）。
- 设置「图库」页里可以新建图库（文件夹或 Apple Photos）、打开其他图库、导入导出（`~/Downloads/<名字>-<日期>.pilib.zip`，只含 `library.json`、两个数据库和特征，用 SQLite 备份接口导出，导入时拒绝不认识的文件，在后台进行、卡片上显示阶段并可取消，见[导入任务](ARCHITECTURE_DIRECTION.md#导入任务)；导出定位为可分享，不含出处缓存 `provenance.sqlite` 和导入过的 X 关注线索，设置页的导出说明与完成提示写明这一点，换电脑后要回查画师需重新导入 X 归档，2026-10-12 用户决定，#41）、改文件夹来源的预设（同时改写这个来源所有图的预设）、把图库移出列表（不删除）。桌面 App 没有可打开的图库时另有欢迎页（见上文「首次建库与参考库」）。
- 导出的文件直接放在 `Downloads`。文件名取图库显示名中的合法字符，必要时改写设备保留名或截短。清理后没有可用字符时用「图库」。包内仍保存原显示名。
  原生选择由平台服务提供；桌面走 Tauri、本机 bridge，CLI macOS 保留 `osascript`，其他 CLI 平台手填。单选兼容与显式多选协议见 [三层边界](ARCHITECTURE_DIRECTION.md#core--platform-service--app-边界)。
- 已知没做：名字缓存挪成所有图库共享（见第 8 节）。
- **测试图库是冻结的快照**（`library.json` 有 `test` 字段）：打开时强制关闭同步和外部角色索引检查。
- Finder 里 `.pilib` 目前显示成普通文件夹；App 还没有把它登记成「包」类型。

## 0. 要解决什么

- 起初（2026-09-27 前）只有一个图库，数据写死在 `review/` 下，来源只有 Apple Photos。
- 需要：
  1. 多个图库互不影响：用户自己的库、各种测试图库（推活型、连续型……）、以后朋友的库；
  2. 除了 Photos（Mac 上 iCloud 照片也经由它），还能**选一个文件夹导入**：Windows、安卓导出的图、下载文件夹；
  3. 为溯源模块（`provenance.sqlite`）和移动端的「资产 ID 链接化」打好地基。

已定的（2026-09-27 和用户确认）：

- **不做账号**，只做「图库」，思路同 Obsidian 的 vault：用户画像就是一个个图库。
- 一个图库是一个**图库包**：里面是文件夹，外面看起来是一个文件（macOS 的 package，和 `Photos Library.photoslibrary` 一样）。
  不压缩成单文件：数据库要随时读写，压缩包每次开关都要解包重打，大库很慢，写到一半出错还容易整个坏掉。
  单文件只用在「导出 / 导入」上（zip，不含图片）。
- **图片不复制**，图库只是索引（记忆 library-as-index）。**不往 Photos 图库、也不往用户的图片文件夹里写任何东西。**
- **人工结论跟着图库走**，图库之间互不影响；便携文件可把当前人工状态导入另一图库，只补目标从未决定的项（见下文「人工结论快照」）。
- 文件夹来源让用户选**来源预设**：X、Pixiv、其他；不按文件夹名猜，见上文「来源管理」。
- 第一版文件夹来源只做 Mac；数据结构按跨平台设计。

## 1. 图库包里有什么

扩展名为 `.pilib`。

```
我的图库.pilib/
  library.json        图库配置：名字、ID、来源列表、格式版本
  source.sqlite       源库（现在的 pi.db）：资产、识别结果（labels、embeddings）
  library.sqlite      应用库（现在的 library.sqlite）：身份、人工结论、撤销历史、派生表
  provenance.sqlite   溯源模块自己的库：查询缓存与机器结果，不进导出包
  features/           本图库的 CCIP 特征（现在的 full-features.npy + full-manifest.csv）
  thumbnails/         缩略图缓存（现在的 web-thumbnails/），可以随时删掉重建
  library-backups/    terms-*.sqlite：升级前必需的完整应用库回退副本
  backups/            library-*.sqlite：设置里的手动应用库备份；source-damaged-*.sqlite：抢救留证
    human-conclusions/ 当前人工状态的自动便携快照，不含图片或完整撤销历史
  album-char-map.json 相册名到角色的映射（只有从旧库迁来的图库有）
  .illustash-owner.lock、.illustash-checks.json  会话的文件锁与检查记录（#212）
```

`library.json`：

```json
{
  "format": 1,
  "id": "6F1C…（UUID，建库时生成，永不改变）",
  "name": "我的图库",
  "created_at": "2026-09-28T10:00:00+08:00",
  "sources": [
    {"id": "s1", "kind": "photos", "path": "~/Pictures/Photos Library.photoslibrary",
     "apps": {"com.atebits.Tweetie2": "x", "net.pxv.iphone": "pixiv"}},
    {"id": "s2", "kind": "folder", "path": "/Users/…/Downloads/pixiv", "preset": "pixiv", "preset_origin": "auto"}
  ],
  "test": {"persona": "连续型", "from": "我的图库", "built_at": "…"}
}
```

- `format` 是包格式的版本，和两个数据库各自的 schema 版本分开。打开时先看 `format`，比程序新就拒绝打开（不猜）。
- `preset_origin`：`user`（用户指定）或 `auto`（用户没有选：CLI 不给 `--preset` 时为「其他」；旧图库里按文件夹名猜出的值也是 `auto`）。用户改过的不再被自动值覆盖。
- `test` 只有测试图库有，写明它是从哪个图库、按什么画像切出来的。

### 人工结论快照

成功开库后，宿主在后台生成首份 `.illustash-human.jsonl`。后续人工变更从首次变脏起合并等待 60 秒，两份成功自动文件至少间隔 1 小时；语义内容相同不重复生成。持续编辑不会把首次等待无限延后。自动目录只保留最近 30 份成功快照，表示文件数，不保证覆盖 30 天。失败保留旧快照和待保护状态，5 分钟后重试；轮换失败单独报告，已生成文件仍然有效。关闭图库不等待整库哈希，未保护修订下次开库继续。

快照包含当前有效人工结论、明确收回的状态及必要身份定义，不包含图片、机器载荷、账号配置、文件路径或源库全部撤销历史。导入只补目标未决定的项，已有 yes/no/clear、结构和人工名字优先；一次有效导入对应一步撤销，无改动不建空历史。精确 after 或后续依赖已变化时撤销整体拒绝。后台可重建的提供方显示名和 Danbooru 别名只作文件内描述，导入明确报告未处理，避免撤销误删后来更新的名字。缺少可核实参考版本的旧锚点、无法解析的身份跳过对等也列为未处理。

图片认回要求完整 SHA-256、字节长度和输入形态一致；同库仍须在线核验当前内容，跨库或原 ID 不在时还须唯一匹配。重复内容不猜测，离线目标及未核验候选会限制认回。来源离线时，仍绑定历史版本的缓存可标为 `last_known`；没有可信摘要的记录仍保留在文件中，报告 `hash_coverage.missing` 和 `portable_complete=false`。原图与派生输入、不同编码或元数据改变后的文件不保证认回。覆盖完整只表示文件内图片身份齐全，不承诺所有依赖都可恢复。

缓存按来源版本、字节长度和 mtime 核验，不依赖 inode 或卷标识；观察到版本变化后旧缓存失效。自动快照只使用已有摘要，随后后台补算；补齐的摘要进下一份快照，仍遵守一小时间隔。例外是每次开库后的第一轮补算把最近一份快照缺的摘要全部补齐时，马上再生成一份，不等一小时（#516：从 v8 升级的图库没有摘要缓存，第一份快照全缺摘要）；每次开库最多一次。设置页把「还没有内容摘要，正在补算」和「补算后仍读不到」分开说，后者才提示从这份快照恢复时那些图认不回来。现行识别 manifest 没有可独立核实的文件版本/时间戳绑定，因此不能仅凭其中的摘要复用；首次需要时读取完整文件。识别主流程不为本功能增加一次全库读取。

手动导出默认写到 Downloads，形成可独立保存的文件；整包删除后可用它向重建图库补回能认回的结论。包内自动快照与图库位于同一存储位置，删包或磁盘损坏会一起丢失。完整 SQLite 备份用于恢复整个应用库及历史，便携快照用于恢复当前人工状态；升级前 `library-backups/` 仍是硬性门槛。自动轮换不会删除手动整库备份、损坏库留证或升级回退副本。设置里的旧备份列表和份数仍只算 `backups/library-*.sqlite`，最多显示 20 条；同一处的说明和「备份放在哪里」写明 `backups/` 与 `library-backups/` 两个文件夹的位置，说清升级前的备份不算在份数里（#517）。

宿主通过 `/api/human-conclusions` 提供状态、导出、导入、恢复和完整分页报告；界面入口单独接入，接口约定见 [核心数据模型](TERMS_SCHEMA.md#15-第-9-版明确收回的人工状态)。

## 2. 包外的共享资源

和具体图库无关、所有图库共用一份的，放在共享资源目录（`ILLUSTASH_SHARED`；未设置时为资源根下的 `review/`）。

最近图库记录、设置和账号放在应用数据目录，由原生 `app_paths::user_home() -> Result<PathBuf>` 与 `app_paths::app_support(home: &Path) -> PathBuf` 统一取值；`app_libraries` 负责最近图库登记。

最近图库、语言偏好和更新设置各自在同名 `.lock` 文件的独占锁下读取、修改并写回。锁被占用时最多等待 5 秒，无法取得锁就报错，原配置不写入。图库包已创建或导入时，登记失败只影响最近图库列表。

非 Windows 主目录取非空 `HOME`，Windows 取非空 `USERPROFILE`，缺失或空值返回「无法定位用户主目录」。macOS 和 Linux 均保留 `~/Library/Application Support/Illustash/`；Linux 尚未迁移到 XDG，改变目录需另行处理已有数据。Windows 为 `%APPDATA%\Illustash\`，`APPDATA` 缺失或空值时回退到主目录下的 `AppData\Roaming\Illustash`。`ILLUSTASH_APP_SUPPORT` 只要存在就原样覆盖（包括空值和相对路径），不建目录、不规范化路径。此接口搬迁不移动已有数据，回退程序沿用相同目录规则。Windows 普通路径规范化也归 `app_paths`；SQLite 长路径及 `win32-longpath` VFS 仍由 `sqlite` 独立封装，详见 [Windows 运行时说明](BACKEND_TASKS.md#windows)。

### 目录职责与输出约定

| 类别与位置 | 实际入口 / 消费者 | 读写与生命周期 | 交付方式 |
|---|---|---|---|
| `crates/`、`native/terms/`、`desktop/` | 业务核心、原生宿主、桌面外壳 | 已跟踪源码，按现有边界维护 | 构建程序；不按目录名退役 |
| `web/static/`、`tags/tags_v3.csv` | 桌面 / HTTP 界面、识别标签 | 安装资源，只读 | 基础包白名单 |
| `models/` | 识别、CCIP 可选计算、基础角色索引（`character-index/`） | 独立获取；不属于 SHARED | 可选完整计算包按白名单携带，基础包不携带 |
| `review/reference.sqlite` | `app_service` 建库参考 | 只读输入；允许缺席，不允许坏库静默回退 | 不随基础包；与 #150 的独立参考索引交付对接 |
| `review/char-index/`、`game-chars/`、`char-anchors.npy`、`char-anchors.txt`、`char-index-base.json` | 原生索引编排、识别与研究 worker | 共享索引 / 锚点；索引更新可写，不能当实验缓存清掉。锚点含已启用的基础角色索引 | SHARED；不随安装包，由索引任务生成 |
| `review/char-patch/` | `app_index` 的 watch、state、更新临时区和租约 | 跨运行持久状态，也含可重新获取的中间产物；须逐对象判断 | SHARED；不随包 |
| `<图库>.pilib/` | `app_library`、同步、缩略图、升级备份 | 用户拥有；source / library 双库、特征、缩略图与备份随包保存 | 用户数据，不进安装包 |
| `APP_SUPPORT/libraries.json`、`preferences.json`（界面语言、检查更新的选择）、`updates.json`（上次检查更新的结果）、设置与账号 | `app_libraries`、`app_preferences`、设置与原生账号入口 | 用户应用数据，和 SHARED 分开 | 不随包；凭据不参与研究整理 |
| `review/identity-metadata-*`、`identity-bank-*`、评测与测试图库输出 | 已删除的身份研究工具、`eval_bench.py`、`test_libraries.py`（#186） | 研究输入或输出，可能含私有 UUID / 文件引用；已有数据保留 | 不随包；新输出显式指定 `_scratch/research/` |
| `review/pi.db`、`library.sqlite`、`full-features.npy`、`full-manifest.csv` 的旧布局入口 | 研究输出与裸库的旧布局 | 可能是指向个人图库的链接；只看链接类别不能推定可删除 | 不随包；不跟随链接做清理 |
| `tools/` | 构建、检查与验收脚本 | 按入口和实际调用判断职责；保留现行消费者 | Python 源码不进任何安装包 |
| `tests/fixtures/`（含 `reference_recordings/`）、契约夹具 | 录下的旧参考固定期望与合成验收 | 已跟踪安全测试输入；不同于本机 `reference/` | 不随生产包，保留回归用途；参考已删除，不能重录 |
| `.venv/`、`target/`、`packaging/desktop-*` | 依赖、构建与固定路径验收 | 本机生成物；正式验收有独立租约与结果签名 | 忽略，不随源码提交；在持有者释放前不整理 |
| `.claude/worktrees/`、`.worktrees/` | 任务代理 checkout | Git 管理的任务生命周期；不能按名称或大小删除 | 忽略，由任务持有者收尾 |

`ILLUSTASH_SHARED` 的绝对路径直接指定共享根；相对路径统一以 `ILLUSTASH_ROOT` / 安装资源根为基准，未设置时仍用该根的 `review/`。原生建库、索引和研究锚点使用同一规则；原生识别与安装 worker 将解析后的绝对共享根传给计算进程，不依赖其工作目录。裸 source 数据库仍按旧布局从数据库同目录取共享数据，图库包才使用 SHARED。

旧版本的索引入口曾把相对共享路径按当前工作目录解释。若旧位置已有 `reference.sqlite`、角色索引、锚点或 `char-patch`，且与新解析位置不同，程序要求把原目录写成绝对 `ILLUSTASH_SHARED`，不切换到空目录。符号链接指向同一目录时视为同一位置。此规则不自动移动或删除数据；回退旧版本前同样使用绝对配置，可保留原路径行为。完整安装的共享可写默认位置仍由安装集成设置，见 [安装说明](BACKEND_TASKS.md#独立安装与可选计算包)。

评测台与测试图库生成工具已于 2026-10-05 删除（#186）。已有研究输出、冻结快照和测试图库继续保留；其目录不再表示存在对应的生成命令。保留工具的用途见 [开发说明](DEVELOPMENT.md#tools-目录)，新研究输出使用显式目录，避免覆盖已有输入与结果。

整理时先核对实际消费者，再确认数据所有者、可重建性及回退位置。未入包、大文件或旧名字都不是删除条件。现有共享资源和历史研究数据保留原位；物理迁移、合并和删除须在 Issue 中按对象决定，不能用空间清理代替源码退役。


## 3. 资产 ID 和链接

BACKLOG 里写着「做移动端之前必须完成资产 ID 链接化」，这次顺手做掉，但**尽量不动现有数据**：

- 所有表里的 `uuid` 列，含义从「Photos UUID」改为「图库内部的资产 ID」，**列名不改**。
  - 来自 Photos 的图：内部 ID 就等于 Photos UUID，**现有数据不用迁移**；
  - 来自文件夹的图：新生成一个 UUID（同样的大写格式）。
- 源库新增一张链接表，记录这张图在外部的各种身份：

```sql
CREATE TABLE asset_links(
  asset TEXT NOT NULL,          -- 内部资产 ID
  system TEXT NOT NULL,         -- 'photos'（Photos UUID）| 'file'（文件夹内相对路径）| 'md5'（原文件内容哈希）
                                -- 以后：'phcloud'（iOS PHCloudIdentifier）| 'android'（MediaStore 路径加哈希）
  key TEXT NOT NULL,
  source_id TEXT NOT NULL,      -- library.json 里的来源 ID
  current INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY(system, key, source_id, asset));
CREATE UNIQUE INDEX asset_links_location ON asset_links(system, key, source_id) WHERE system<>'md5';
CREATE INDEX asset_links_asset ON asset_links(asset);
```

- `assets` 表加两列：`source_id`（来自哪个来源）、`preset`（`x` / `pixiv` / `other`）。
  `preset` 是后面几件事的依据：「以图搜图」只发 `x`、`pixiv` 的图；溯源回查按来源选 Danbooru 还是 Pixiv；界面上按来源筛选。
  - Photos 来源：由 `library.json` 里 `apps` 的映射决定（原生来源实现见 `crates/core/src/origin.rs` 与 `crates/platform/src/platform_photos.rs`）；
  - 文件夹来源：取该来源的 `preset`。
- 首次扫描有文件记录的来源时，在源库事务里扩展链接主键，按 `files.md5` 补写每个文件的内容链接。文件路径和 Photos 标识在同一来源内仍然唯一，同一 MD5 可指向多个独立资产。普通同步沿用原位编辑的 ID；指定新根目录时逐文件校验内容，规则见上文「来源管理」。
- `source.sqlite` 损坏时，文件夹图片的 uuid 靠 `library.sqlite` 认回（#228）：它的 `assets.src_path` 记着每张在库图片的完整路径，`meta` 里的 `folder_hash:<uuid>` 记着内容 md5（图片移出图库后也保留）。`library.sqlite` 的每份备份因此都带着与其中整理记录一致的对照表；应用内抢救按它重扫，见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md) 开库检查一节。
- `library.sqlite` 损坏时只能换回整库备份（#492，损坏提示 `app_integrity::LIBRARY_DAMAGED`）：退出应用，把它连同 `-wal`/`-shm` 移走，复制一份备份改名为 `library.sqlite`。可用的是 `backups/library-*.sqlite`（设置里的手动备份，当前格式，直接打开）和 `library-backups/terms-*.sqlite`（升级前的必需备份，内容停在升级前，打开时再升级一次；需要 pre-v8 版本升级的不能用）；`backups/source-damaged-*.sqlite` 和 `backups/human-conclusions/` 里的快照不是应用库。选修改时间最近的一份，之后的整理丢失；打开后可以在「设置 › 数据」的「人工结论快照」里对换回之前最近的一份自动快照点「恢复…」，补回备份之后的人工结论（只补仍未决定的项，冲突时保留已有的，见上文「人工结论快照」）；换回的图库打开后也会生成快照，内容是换回后的状态，补回时不要选它。记过损坏的图库换回旧格式备份后，打开前的完整检查通过就清除损坏记录，改为提问升级。

## 4. 来源

**Photos 来源**：由 `native/terms/src/app_sync.rs` 协调原生盘点、识别与导入，Photos 读取在 `crates/platform/src/platform_photos.rs`；图库路径和 App 映射从 `library.json` 读取。

**文件夹来源**：

1. 用户选一个文件夹并指定出处（X / Pixiv / 其他），不按文件夹名猜。
2. 递归扫描图片文件（jpg、jpeg、png、webp、gif），跳过隐藏文件、系统文件和 `.pilib` 子目录。
3. 每张算 `md5`，写进 `assets` 和 `asset_links`，`src_path` 就是文件本身。
4. 识别由可选识别 worker 按路径完成；没有 worker 时照常盘点导入，状态为待识别（见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「原生来源同步与应用刷新」）。原来超过 `startup.local_limit` 就提示交给 Windows 的上限已删除（#100）。
5. 之后每次打开图库按启动偏好做一次增量重扫：新文件入库，消失的文件标记为已删除（和 Photos 里删图一样处理）。不做实时监听。

一个图库可以有多个来源，规则见上文「来源管理」。

## 5. 测试图库变成真图库

测试图库生成器已于 2026-10-05 删除（#186），不再提供 `--materialize` 命令。已有 `.pilib` 测试图库仍可按普通图库打开、切换和登记，包内的 `test` 字段继续供开发模式显示；工具退役不删除这些数据。合成验收使用本文开头列出的临时测试夹具。

## 6. 启动、切换与界面

- 原生宿主用 `--library <图库包>`；不给时打开最近图库，没有最近图库时提示新建首库（见「首次建库与参考库」）。原来的 `--source`、`--db` 宿主参数已退役。
- 同一时间只开一个图库。切换时停掉当前图库的后台任务（同步、名字拉取、相似检测、学习），关掉数据库，再打开新图库；旧会话的请求和任务结果不能写进新图库。
- 设置页「图库」卡片管理图库和来源：新建（给个名字 → 选来源：Photos，或选文件夹并确认预设 → 建好后打开并同步；没有可选识别 worker 时仍完成图片盘点与导入，状态为待识别）、切换、导入导出、改文件夹预设。

## 7. 和 Rust 的关系

用户于 2026-10-01 批准稳定应用完全由 Rust 运行；研究 worker 也已原生化（#9）。图库生命周期、建库（文件夹 / Photos，Photos 创建不读取来源或启动同步）、图库选择、来源预设、最近列表与归档进出都由原生服务处理；完整行为、任务收尾和回退见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「原生图库管理与归档」。Photos 只读盘点、混合来源同步及可选识别结果编排见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「原生来源同步与应用刷新」；Windows 选择窗口与 worker 进程树保持独立平台边界。

图库包目录、版本化 `library.json`、SQLite 和 `.npy` 是跨语言契约，原生宿主直接打开同一包、自持连接；普通查询与建库、升级、备份和扫描严格分离。切库必须停止旧任务、释放连接，并通过图库身份/会话 generation 阻断旧请求和 worker 结果。格式或 schema 需要不兼容变更时，先备份、独立迁移验收，并从备份回退，不能承诺无需数据转换。

## 8. 还没定的

1. 扩展名 `.pilib` 先这么叫，产品名定了再换。
2. 名字缓存等外部查询缓存什么时候挪成所有图库共享：`name_cache` 等现在在 `library.sqlite` 里，每个图库各自拉取；挪成共享要单独设计。

现行数据库兼容结构及消费者见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13 节；显式升级、成对发布与备份回退边界见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「已有核心库的显式升级边界」。源库研究/历史附加表及隐式 rowid 保留；注册失败和提交后事件失败须按已提交状态恢复；打开时登记最近图库失败不挡住打开（#176），原因见 `/api/libraries` 的 `unlisted`。

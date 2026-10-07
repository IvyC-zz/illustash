# 图库：多图库与文件夹来源

状态：现行参考，2026-10-03。图库来源与包结构；安装资源及平台限制见 [BACKEND_TASKS.md](BACKEND_TASKS.md)。

## 首次建库与参考库

基础版不携带 `reference.sqlite` 或个人图库数据。桌面 App 第一次启动、没有能打开的图库时显示欢迎页，在图形界面里新建图库（见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md) 的外壳启动段落）。命令行首次使用可执行 `bin/illustash-terms library-create --dest <新包.pilib> --folder <图片目录>`（Windows 可执行文件为 `.exe`），再用 `bin/illustash-terms http --library <新包.pilib>` 打开；服务启动仍需已初始化的图库，不会隐式创建。CLI 不给 `--reference` 时直接使用程序内置的空 source schema，角色、别名、模型运行、图片、标签、决策和 embeddings 均为空；建库不扫描图片，也不启动计算 worker。

设置页新建图库默认查看资源根下的 `review/reference.sqlite`（设置 `ILLUSTASH_SHARED` 时改看该目录）。仅路径不存在时使用内置空 schema；已有但损坏、不完整、不可读的参考库和断链不会静默回退。显式 CLI `--reference` 必须指向有效文件。参考库只读复制表结构与索引，只复制 `characters`、`character_aliases`、`runs` 的行，不复制图片或人工决策；它代表导出时的角色索引，不自动跟随原图库更新。

新包发布 `library.json` 前校验盘点、读取、导入所需列，包括 `assets.hidden/stable_hash/orig_size`、`embeddings.uuid/model/vec`、`labels.source/model_ver/run_id` 等标签导入字段和角色索引字段；照片来源另校验 `assets.z_pk`。保留原有 `source_id/preset` 和文件盘点表的创建兼容，其余不完整参考库拒绝发布并清理本次新包，不改参考输入。处理办法是省略 CLI 的 `--reference` 使用空 schema，或提供符合当前结构的参考库；已有图库不由此入口升级。

2026-10-01 在 macOS arm64 上用白名单基础产物和隔离用户目录实测：无共享资源或参考库时，CLI 首库、HTTP 新建与打开、首次同步以及图片内容变更后的同步均通过；只用合成图片。此结果不代表 Windows 干净机或桌面安装验收（仍由 #71 / #90 承接）。

基础版首次文件夹同步完成盘点和应用导入后可浏览、人工整理；没有可选识别 worker 时状态为「待识别」，不表示盘点失败。回退时使用同版本程序和资源，旧版仍可能要求外部参考库；不要为回退把个人数据放进安装资源。

合成测试输入由 `tests/synthetic_source.py` 的 `source_fixture(root)` 与 `synthetic_reference(root)` 构造，只使用显式临时目录和 SQLite/JSON，不导入 Store/Server 或调用原生候选建库。前者保留固定小源库及相册映射，后者保留完整参考 schema 与 `characters`、`character_aliases`、`runs` 行；历史升级 schema/producer 和业务期望仍由各自既有测试持有。旧的 `tests.test_web.fixture` 已随第 1 类测试删除（#9），各测试直接导入 `source_fixture`；仍依赖动态 Store/Server 的对照按 #9 后续阶段退役。

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
- **设置页界面**：设置「图库」页（`#/settings/library`）里当前图库的来源按出处分组（Pixiv / X / 其他，macOS 另有 iCloud 照片），每组可以「添加文件夹…」（原生窗口多选），每个文件夹可以改出处或移除；移除前弹窗写明会移除的图片数。「新建图库」按出处分三栏添加文件夹，macOS 可勾选同时读取 iCloud 照片。没有原生选择窗口的环境（非 macOS 的 CLI 网页模式）改为输入路径。
- **移除来源**：`POST /api/libraries/sources/remove {source}` 先只返回会移除的图片数（`{confirm: true, count}`），界面据此显示页面内确认框并等待明确确认，取消、关闭或 Esc 不提交；不依赖桌面插件替换后的 `window.confirm`，也不增加图库页面的 Tauri 权限。重复点击同一来源只保留一个确认流程；确认期间导航或会话改变会使确认失效，提交仍绑定预览时的会话 token。带 `confirm: 1` 才执行。执行时先把这个来源的图在源库里标为已删除（与文件消失相同），再把来源从 `sources` 挪到 `library.json` 的 `removed_sources`（带 `removed_at`），然后开始一次同步，把删除带进浏览用的应用库。人工结论不物理删除。图库至少保留一个来源。
- **重新添加同一个文件夹**会沿用原来的来源 id，同步时按 `files` 表里的记录把这些图恢复（资产 ID 和人工结论不变）；可以换一个出处，这个来源的图随之改出处。
- 同步或角色索引更新进行中时拒绝修改来源；测试图库（冻结快照）不能修改来源。
- 回退：旧版本不认识 `removed_sources`，会忽略它，已移除来源的图仍是已删除状态；旧版本仍可打开和同步其余来源。

图片详情的「文件形态」显示原图或本地副本；文件夹与 Photos 共用「来源同步」入口。

## 多图库与来源约定

L0–L3 的现行约定（2026-09-28）：
- L0：`web/library.py`、server 的 `--library`、现有数据迁成 `~/Pictures/Illustash/我的图库.pilib`。
- L1：源库加了 `asset_links`、`assets.source_id`、`assets.preset`（打开图库时自动升级，Photos 可读时按 App 回填预设；同步新图时一并登记）；
  `tools/test_libraries.py --materialize <名字>`（2026-10-05 删除，#186；见 git 历史）把测试清单生成成图库包（3,000 张约 11 秒、390 MB）；
  **图库切换放在设置的「图库」页**（用户 2026-09-27 定：切换不频繁，不放侧栏），切换 = 后台用同一端口重启到另一个图库；
  侧栏只在当前是测试图库时在 Logo 下显示一行小字。
- L2：`tests/reference/web/folder.py` 文件夹扫描（只对大小或修改时间变了的文件算 MD5；按 MD5 跟上移动和改名，资产 ID 和人工结论不变；
  消失的文件标记已删除，同一路径内容变了就重新识别）；`files` 表记录每个文件上次读到的样子；`Store.refresh_paths()` 在每次同步后把新路径同步进应用库。
  新建命令和参考库行为见上文「首次建库与参考库」；出处不再按文件夹名猜（见上文「来源管理」）。
  实测：30 张图的文件夹建库、识别（每张约 0.8 秒），人工「移出角色」后把文件改名移到子文件夹，重新同步后资产 ID、人工结论、图片显示都正常。
- L3：设置「图库」页里可以新建图库（文件夹或 Apple Photos，默认存在 `~/Pictures/Illustash/`，重名自动加序号）、
  打开其他图库、导入导出（`~/Downloads/<名字>-<日期>.pilib.zip`，只含 `library.json`、两个数据库和特征，用 SQLite 备份接口导出，
  导入时拒绝不认识的文件）、改文件夹来源的预设（同时改写这个来源所有图的预设）、把图库移出列表（不删除）。
  原生选择由平台服务提供；桌面走 Tauri、本机 bridge，CLI macOS 保留 `osascript`，其他 CLI 平台手填。单选兼容与显式多选协议见 [三层边界](ARCHITECTURE_DIRECTION.md#core--platform-service--app-边界)。
- 已知没做：名字缓存挪成所有图库共享。一个图库多个来源的后端见上文「来源管理」，界面由 #107 后续 PR 完成。
- **测试图库是冻结的快照**（`library.json` 有 `test` 字段）：打开时强制关闭同步和外部角色索引检查。
  第一次实测时没有这条，同步把 Photos 里另外 4.5 万张写进了测试图库，已修复并重新生成。

L0 实际做法和设计稿的差别：
- 迁移后在 `review/` 下留了软链接（`pi.db`、`library.sqlite`、`full-features.npy`、`full-manifest.csv`），`tools/` 里还读 `review/pi.db` 的研究脚本不用改就能继续用；
  以后逐个改成接收 `--library`，全部改完再删软链接。
- 共享资源开发期仍在 `review/`（`web.library.SHARED`，可用环境变量 `ILLUSTASH_SHARED` 改）；只有图库包里的源库才去 SHARED 找，
  裸的源库（测试、旧用法）仍在它旁边找，所以测试不受影响。
- `Store` 构造函数里的迁移等副作用还没拆（原计划 L1）；它不影响多图库功能，放到做 Rust 核心之前单独做。
- 在还没有安装的 App 登记 `.pilib` 这个类型之前，Finder 里 `.pilib` 显示成普通文件夹；打包成 App 时再登记成「包」。

## 0. 要解决什么

- 现在只有一个图库，数据写死在 `review/` 下（`pi.db`、`library.sqlite` 等），来源只有 Apple Photos。
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
- **人工结论跟着图库走**，图库之间互不影响；以后需要再做「从另一个图库导入人工结论」。
- 文件夹来源让用户选**来源预设**：X、Pixiv、其他；不按文件夹名猜，见上文「来源管理」。
- 第一版文件夹来源只做 Mac；数据结构按跨平台设计。

## 1. 图库包里有什么

扩展名暂定 `.pilib`（产品名定了再改）。

```
我的图库.pilib/
  library.json        图库配置：名字、ID、来源列表、格式版本
  source.sqlite       源库（现在的 pi.db）：资产、识别结果（labels、embeddings）
  library.sqlite      应用库（现在的 library.sqlite）：身份、人工结论、撤销历史、派生表
  provenance.sqlite   溯源模块自己的库（第一阶段再建）
  features/           本图库的 CCIP 特征（现在的 full-features.npy + full-manifest.csv）
  thumbnails/         缩略图缓存（现在的 web-thumbnails/），可以随时删掉重建
  backups/            升级前的自动备份（现在的 library-backups/）
  album-char-map.json 相册名到角色的映射（只有从旧库迁来的图库有）
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

## 2. 包外的共享资源

和具体图库无关、所有图库共用一份的，放在共享资源目录（`ILLUSTASH_SHARED`；未设置时为资源根下的 `review/`）。

最近图库记录、设置和账号放在应用数据目录，由原生 `app_paths::user_home() -> Result<PathBuf>` 与 `app_paths::app_support(home: &Path) -> PathBuf` 统一取值；`app_libraries` 负责最近图库登记。

非 Windows 主目录取非空 `HOME`，Windows 取非空 `USERPROFILE`，缺失或空值返回「无法定位用户主目录」。macOS 和 Linux 均保留 `~/Library/Application Support/Illustash/`；Linux 尚未迁移到 XDG，改变目录需另行处理已有数据。Windows 为 `%APPDATA%\Illustash\`，`APPDATA` 缺失或空值时回退到主目录下的 `AppData\Roaming\Illustash`。`ILLUSTASH_APP_SUPPORT` 只要存在就原样覆盖（包括空值和相对路径），不建目录、不规范化路径。此接口搬迁不移动已有数据，回退程序沿用相同目录规则。Windows 普通路径规范化也归 `app_paths`；SQLite 长路径及 `win32-longpath` VFS 仍由 `sqlite` 独立封装，详见 [Windows 运行时说明](BACKEND_TASKS.md#windows)。

### 目录职责与输出约定

| 类别与位置 | 实际入口 / 消费者 | 读写与生命周期 | 交付方式 |
|---|---|---|---|
| `crates/`、`native/terms/`、`desktop/` | 业务核心、原生宿主、桌面外壳 | 已跟踪源码，按现有边界维护 | 构建程序；不按目录名退役 |
| `web/static/`、`tags/tags_v3.csv` | 桌面 / HTTP 界面、识别标签 | 安装资源，只读 | 基础包白名单 |
| `models/` | 识别、CCIP 可选计算 | 独立获取；不属于 SHARED | 可选完整计算包按白名单携带，基础包不携带 |
| `review/reference.sqlite` | `app_service` 建库参考、Python 图库参考入口 | 只读输入；允许缺席，不允许坏库静默回退 | 不随基础包；与 #150 的独立参考索引交付对接 |
| `review/char-index/`、`game-chars/`、`char-anchors.npy`、`char-anchors.txt` | 原生索引编排、识别与研究 worker | 共享索引 / 锚点；索引更新可写，不能当实验缓存清掉 | SHARED；不随基础包 |
| `review/char-patch/` | `app_index` 的 watch、state、更新临时区和租约 | 跨运行持久状态，也含可重新获取的中间产物；须逐对象判断 | SHARED；不随包 |
| `<图库>.pilib/` | `app_library`、同步、缩略图、升级备份 | 用户拥有；source / library 双库、特征、缩略图与备份随包保存 | 用户数据，不进安装包 |
| `APP_SUPPORT/libraries.json`、设置与账号 | `app_libraries`、设置与原生账号入口 | 用户应用数据，和 SHARED 分开 | 不随包；凭据不参与研究整理 |
| `review/identity-metadata-*`、`identity-bank-*`、评测与测试图库输出 | 已删除的身份研究工具、`eval_bench.py`、`test_libraries.py`（#186） | 研究输入或输出，可能含私有 UUID / 文件引用；已有数据保留 | 不随包；新输出显式指定 `_scratch/research/` |
| `review/pi.db`、`library.sqlite`、`full-features.npy`、`full-manifest.csv` 的旧布局入口 | 研究工具与裸库兼容入口 | 可能是指向个人图库的链接；只看链接类别不能推定可删除 | 不随包；不跟随链接做清理 |
| `tools/`、`provenance/`、`web/*worker.py` | 构建、验收、研究或打包白名单中的计算源码 | 按入口和实际调用判断职责；保留现行消费者 | 部分源码进可选 workers 包；算法迁移另行处理 |
| `tests/reference/`、契约夹具 | 历史逐字节对照与合成验收 | 已跟踪安全测试输入；不同于本机 `reference/` | 不随生产包，保留回归用途 |
| `.venv/`、`target/`、`packaging/desktop-*` | 依赖、构建与固定路径验收 | 本机生成物；正式验收有独立租约与结果签名 | 忽略，不随源码提交；在持有者释放前不整理 |
| `.claude/worktrees/`、`.worktrees/` | 任务代理 checkout | Git 管理的任务生命周期；不能按名称或大小删除 | 忽略，由任务持有者收尾 |

`ILLUSTASH_SHARED` 的绝对路径直接指定共享根；相对路径统一以 `ILLUSTASH_ROOT` / 安装资源根为基准，未设置时仍用该根的 `review/`。原生建库、索引、研究锚点和 Python 图库辅助入口使用同一规则；原生识别与安装 worker 将解析后的绝对共享根传给计算进程，不依赖其工作目录。裸 source 数据库仍按旧布局从数据库同目录取共享数据，图库包才使用 SHARED。

旧的索引和 Python 入口曾把相对共享路径按当前工作目录解释。若旧位置已有 `reference.sqlite`、角色索引、锚点或 `char-patch`，且与新解析位置不同，程序要求把原目录写成绝对 `ILLUSTASH_SHARED`，不切换到空目录。符号链接指向同一目录时视为同一位置。此规则不自动移动或删除数据；回退旧版本前同样使用绝对配置，可保留原路径行为。完整安装的共享可写默认位置仍由安装集成设置，见 [安装说明](BACKEND_TASKS.md#独立安装与可选计算包)。

评测台与测试图库生成工具已于 2026-10-05 删除（#186）。已有研究输出、冻结快照和测试图库继续保留；其目录不再表示存在对应的生成命令。保留工具的用途见 [TOOLS.md](TOOLS.md)，新研究输出使用显式目录，避免覆盖已有输入与结果。

整理时先核对实际消费者，再确认数据所有者、可重建性及回退位置。未入包、大文件或旧名字都不是删除条件。现有共享资源和历史研究数据保留原位；物理迁移、合并和删除须在 Issue 中按对象决定，不能用空间清理代替源码退役。
**第一版不动的**：名字缓存（`name_cache` 等）现在在 `library.sqlite` 里，每个图库各自拉取。挪成共享要单独设计，先不做。

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
  PRIMARY KEY(system, key, source_id));
CREATE INDEX asset_links_asset ON asset_links(asset);
```

- `assets` 表加两列：`source_id`（来自哪个来源）、`preset`（`x` / `pixiv` / `other`）。
  `preset` 是后面几件事的依据：「以图搜图」只发 `x`、`pixiv` 的图；溯源回查按来源选 Danbooru 还是 Pixiv；界面上按来源筛选。
  - Photos 来源：由 `library.json` 里 `apps` 的映射决定（原生来源实现见 `crates/core/src/origin.rs` 与 `crates/platform/src/platform_photos.rs`）；
  - 文件夹来源：取该来源的 `preset`。
- 同一张图跨设备、跨来源时靠 `md5` 对上；文件被移动或改名时，重扫先按路径找，找不到再按 `md5` 找，都找不到才算新图。

## 4. 来源

**Photos 来源**：由 `native/terms/src/app_sync.rs` 协调原生盘点、识别与导入，Photos 读取在 `crates/platform/src/platform_photos.rs`；图库路径和 App 映射从 `library.json` 读取。`tests/reference/web/sync.py` 只用于历史参考对照。

**文件夹来源**（新）：

1. 用户选一个文件夹并指定出处（X / Pixiv / 其他），不按文件夹名猜。
2. 递归扫描图片文件（jpg、jpeg、png、webp、静态 gif），跳过隐藏文件和系统文件。
3. 每张算 `md5`，写进 `assets` 和 `asset_links`，`src_path` 就是文件本身。
4. 识别沿用 `web/infer.py`（它本来就只认路径），超过 `LOCAL_LIMIT` 的首次全量照旧提示交给 Windows。
5. 之后每次打开图库做一次增量重扫：新文件入库，消失的文件标记为已删除（和 Photos 里删图一样处理）。第一版不做实时监听。

一个图库可以有多个来源，规则见上文「来源管理」。

## 5. 测试图库变成真图库

测试图库生成器已于 2026-10-05 删除（#186），不再提供 `--materialize` 命令。已有 `.pilib` 测试图库仍可按普通图库打开、切换和登记，包内的 `test` 字段继续供开发模式显示；工具退役不删除这些数据。合成验收使用本文开头列出的临时测试夹具。

## 6. 程序怎么改

**生命周期先拆开**（对应 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「推进顺序」M1）：

- 新增 `web/library.py`：负责图库包的**建立、打开、升级迁移、备份**，以及读写 `library.json`；
- `Store` 只接收一个已经打开的图库，不再在构造函数里顺带做迁移、读相册标题、补索引这些副作用；
- 平台来源（Photos、文件夹）拆成独立的扫描器，只负责「列出资产、给出路径和链接」，不直接写应用库。

**启动和切换**：

- 原生宿主用 `--library <图库包>`；不给时打开最近图库，没有最近图库时提示用 CLI 新建首库（见「首次建库与参考库」）。原来的 `--source`、`--db` 宿主参数已退役。
- 同一时间只开一个图库。切换时停掉当前图库的后台线程（同步、名字拉取、相似检测、学习），关掉数据库，再打开新图库。
  第一版可以简单做成「切换 = 重启后端里的图库对象」，不要求无缝。

**界面**：

- 侧栏最上面显示当前图库名，点开是菜单：最近打开的图库、打开图库…、新建图库…。
- 已打开图库后，在设置页新建：给个名字 → 选来源（Photos，或选一个文件夹并确认预设）→ 打开新库并同步。没有可选识别 worker 时仍完成图片盘点与导入，状态为待识别。
- 设置页的「收录来源」一栏改成列出本图库的来源，可以改文件夹的预设。

## 7. 迁移现有数据

1. 先用 SQLite 备份接口做一次完整备份（现有的 `--backup`）。
2. 新建 `~/Pictures/PhotoIntelligence/我的图库.pilib`（默认位置，可改），把 `review/` 下这些**移动**进去（不复制，`pi.db` 有 5.8 GB）：
   `pi.db` → `source.sqlite`，`library.sqlite`，`web-thumbnails/` → `thumbnails/`，`library-backups/` → `backups/`，
   `full-features.npy` 和 `full-manifest.csv` → `features/`，`album-char-map.json`。
3. 写 `library.json`：一个 Photos 来源，`apps` 取现在的 `COLLECT`。
4. 两个数据库各升一个 schema 版本：加 `asset_links`、`assets.source_id`、`assets.preset`，并按 Photos UUID 回填链接。
5. `review/` 下的研究产物（评测结果、溯源实验、角色索引构建中间文件）留在原处，不属于图库。
6. `tools/` 里写死 `review/pi.db`、`review/library.sqlite` 的脚本（`eval_bench.py` 等）改成接收 `--library`。

验收：迁移前后用正式库副本对比每张表的行数和人工结论，界面上浏览、改角色、撤销都正常，启动时间不变慢。

## 8. 和 Rust 的关系

用户于 2026-10-01 批准稳定应用完全由 Rust 运行，Python 只保留可选研究 worker；统一设计与阶段门槛见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「推进顺序」。图库生命周期在 M1 接管，导入导出与文件服务在 M3 接管；显式原生服务已支持完整 fresh 文件夹/Photos 包创建、登记和打开，Photos 创建不读取来源或启动同步。图库选择、来源预设、最近列表与归档进出也已由原生服务处理；完整行为、任务收尾和回退统一见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「原生图库管理与归档」。Photos 只读盘点、混合来源同步及可选识别结果编排见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「原生来源同步与应用刷新」；Windows 选择窗口与 worker 进程树保持独立平台边界。

图库包目录、版本化 `library.json`、SQLite 和 `.npy` 是跨语言契约。Rust 应直接打开同一包，自持连接；普通查询与建库、升级、备份和扫描严格分离，不依赖 Store 构造副作用。切库必须停止旧任务、释放连接，并通过图库身份/会话 generation 阻断旧请求和 worker 结果。格式或 schema 需要不兼容变更时，先备份、独立迁移验收，并从备份回退，不能承诺无需数据转换。

## 9. 顺序

| 步 | 内容 | 验收 |
|---|---|---|
| L0 | `web/library.py`：图库包的建立、打开、迁移、备份；server 改为 `--library`；把现有数据迁成「我的图库」；共享资源的路径集中配置 | 行为不变；正式库副本迁移前后逐表一致 |
| L1 | `asset_links`、`source_id`、`preset`；`test_libraries.py --materialize`；侧栏切换图库 | 能在界面里切到测试图库，看到新用户状态 |
| L2 | 文件夹来源：扫描、`md5`、识别、增量重扫、来源预设 | 选一个文件夹能建成图库；移动或改名文件后重扫不丢人工结论 |
| L3 | 新建图库向导、设置页的来源管理、导出/导入（zip，不含图片） | 已打开图库后可在界面建库和切换；首库命令见上文 |

之后溯源第一阶段的 `provenance.sqlite` 放进各自的图库包，评测在测试图库上跑。

## 10. 还没定的

1. 图库包的默认位置：`~/Pictures/PhotoIntelligence/`，还是 `~/Documents/`？（用户随时可以另选。）
2. 扩展名 `.pilib` 先这么叫，产品名定了再换。
3. 名字缓存等外部查询缓存什么时候挪成所有图库共享。

现行数据库兼容结构及消费者见 [TERMS_SCHEMA.md](TERMS_SCHEMA.md) 第 13 节；显式升级、成对发布与备份回退边界见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)「已有核心库的显式升级边界」。源库研究/历史附加表及隐式 rowid 保留，不因宿主退出删除；注册失败和提交后事件失败须按已提交状态恢复。

# v0.1.0 发布后的方向调研（2026-11-14）

状态：调研记录，2026-11-14，不是现行参考。v0.1.0 发布当天做的一次方向调研：盘点开着的 Issue，读现行文档，再分四路调研前端、后端与工程、同类产品与分发、模型与算法进展。第 1 节是汇总和建议路线，第 2–5 节是四份调研的原文（只去掉了本机路径）。用户 2026-11-14 按第 1 节的建议全部采纳：路线写进 [BACKLOG.md](BACKLOG.md#现阶段路线用户-2026-11-14-定)，任务开在里程碑「0.1.1」「0.2 找和逛」和 #439–#459。各 Issue 做完时，本篇里对应的结论并入现行参考；全部覆盖后按 [文档维护规范](../AGENTS.md#文档维护规范) 删掉本篇。

**口径**

- 代码和文档的事实取自 origin/main `230ce2de`（2026-11-14）。代码位置的写法：`nt/` 是 `native/terms/src/`，`cc/` 是 `crates/core/src/`，`cp/` 是 `crates/platform/src/`，`ui/`、`core/` 在 `web/static/` 下；文档缩写见第 3 节开头。
- 外部信息来自 2026-11-14 的联网检索，搜索词里没有本项目的名字。三张关键模型卡（deepghs/siglip_beta、Kaloscope 2.0、PixAI Tagger v0.9）由汇总者复核过，其余只经调研代理读取，标「未核实」的没拿到一手来源。
- 没有跑任何实验、构建或性能测量。「推断」是调研者的判断，不是实测。
- 真实图库只写取整规模（约 5 万张），不写内容构成。

## 1. 汇总与建议路线

### 1.1 结论

产品现在是「整理做得深，找图、逛图、用图做得薄」，而且只在作者一个人的图库上验证过。建议按这个顺序走：

1. **0.1.x：让第二个人用得起来。** 收尾、发 0.1.1、小范围内测。
2. **0.2：把「找和逛」做出来。** 要用的数据都已经在库里，不需要新模型。
3. **认得更全、找回更多画师。** 解冻研究，和第 2 步并行，一边做界面，一边做离线实验。
4. **跨设备放最后。** 先用「手机浏览器访问」试水。

### 1.2 现状

- **发布**：v0.1.0 当天发出。仓库 0 star，安装包下载量是个位数，还没有别人提的反馈。
- **Issue**：开着 35 个，都已分派，都是收尾：前端 12 个，工程 16 个，冻结的研究 6 个，发布后验证 1 个。
- **检索**：图库一次只能带一个角色、作品、画师、标签或某一天（`nt/http_browse.rs` 的 `FILTERS`）。不能组合多个标签，不能排除，不能按来源或日期区间筛。8,106 个画面标签只显示英文 tag 名。
- **还没有的**：收藏、保存的筛选、画师目录、「更多像这张的」、打开原文件、拖出、导出选中的图。
- **新用户和作者的体验不一样**：安装包不带共享角色索引（[THIRD_PARTY.md](THIRD_PARTY.md)），新图库只认得 WD 的约 2,700 个角色。没有外部索引时，判别式认领也不启用（[WEB_V2.md](WEB_V2.md#新角色流程相似分组与边标边学2026-09-24)）。
- **规模**：学习、相似扫描这类研究任务，每次运行都把两个数据库整份复制到临时目录（`nt/app_research.rs` 的 `snapshot`），时限固定 30 分钟。现有测量最大只到约 5 万张。
- **数据安全**：人工结论只在 `library.sqlite` 里，只有手动备份和升级前备份。
- **桌面**：关主窗口就退出（`desktop/src/main.rs`），而首次识别 5 万张要几个小时。

### 1.3 产品

- **尽快发 0.1.1**：#421 里的更新流程要有第二个版本才能验。现在用户接近零，出问题的代价最小。
- **内测前补两样**：可选的本地日志（现在用户现场只有错误编号，宿主的报错只在内存里留 30 行）；人工结论的自动备份。
- **分发**：macOS 公证只有 Apple 开发者计划一条路，每年 99 美元，Homebrew 官方库 2026-09 起不收未公证的应用。Windows 不签名时每个新版本的 SmartScreen 信誉从零开始；不花钱的路是 Microsoft Store 加 winget（个人注册免费，由商店签名），商店对 R18 内容的政策要先核实。建议内测阶段不花钱，准备公开宣传前再付公证。
- **差异点**：没找到同时做到「按作品、角色、造型建目录，纠错后本机学习，未知角色聚类，只发哈希找出处」的产品。角色标签本身已经不稀缺，差异在流程和隐私。同类产品被抱怨最多、我们也缺的是手机端。
- **宣传放在 0.2 之后**：先中文（V2EX、小众软件、少数派），再英文。素材全用全年龄图。
- **风险**：Pixiv 2026-06 改了规约，新增禁止机械化批量操作；「不得用程序收集作品」在商标指南里，不是这次新加的。2026-11-15 对照原文复核：关注列表不用改，「找 TA 的其他图」在灰色地带、行为不变，依据见 [PROVENANCE.md](PROVENANCE.md#1-前提与底线) 第 1 节（#442）。

### 1.4 0.2「找和逛」

| 方向 | 现状 | 代价 |
|---|---|---|
| 组合检索：多标签、排除、来源、日期区间 | 每种条件只收一个值 | 后端筛选和地址格式要扩展 |
| 画面标签的中文、日文名 | 只有英文 tag 名 | 有公开别名表，约 10 万条中文译名，MIT |
| 保存的筛选，当作「画廊」 | 筛选条件已全写在地址里；画廊视图 2026-09 起暂缓 | 形态要用户定 |
| 收藏 | 没有 | 新增一种人工标记，按数据层检查 |
| 画师目录 | 画师只能从单张图点进去；`/api/artists` 只统计原创角色上填的画师 | 一个汇总接口加一页 |
| 更多像这张的 | 没有；WD 特征已在库里 | 一个近邻接口，不用重新识别 |
| 打开原文件、在访达中显示、拖出、导出选中的图 | 没有 | 图库页没有原生权限，要经桌面外壳转一道 |
| 后台运行、完成通知 | 关窗即退出 | 要重新定关窗和任务的关系 |

- 小项：缩略图按屏幕倍率选档（现在只看 CSS 像素，2 倍屏偏糊）；修改后只刷新变化的部分（现在每次整份重读，服务端约 0.6 秒）。
- 前端技术路线不用换：吃亏的只有「带输入框、又被轮询反复重画」的面板，一个小的局部更新工具就够。
- 值得把浏览器走查脚本放进仓库：首发前审查找出的 10 条看得见的问题，现有测试一条都没拦住。

### 1.5 算法

| 方向 | 新线索 | 建议 |
|---|---|---|
| 自建标签候选（#157） | 评估已做完，两项待决定也都定了 | 第一个解冻，直接做接入。同一套机制也能支撑「更多像这张的」 |
| 新角色覆盖（#380） | PixAI v0.9 冻结 WD EVA02 主干、只训练分类头，角色 micro-F1 自测 0.865，同口径下 WD v3 约 0.608（已复核） | 先量缺口。先试「发布共享参考特征」，不增加识别耗时，还能让新用户的判别式认领启用；再试在已存特征上加角色头 |
| 画风模型给 Pixiv 提供候选画师（新） | Kaloscope 2.0 分 39,260 位画师，Apache-2.0，10% 验证集 top-1 90.13%；模型卡写明仅供研究和教育用途，分数不代表真实作者（已复核） | 开研究 Issue。只出候选，仍要现有的本地比对确认。评估集现成：库里已有几千张确认过画师的图 |
| 多人图先裁出每个人再认（新） | CCIP 官方说明只适用于单人图；有公开的人物、头部检测模型 | 保留整图 CCIP。长尾人体框在 18 个明确多人目标中多找回 2 个，同时新增 1 个标注外候选（#456，详见 5.2） |
| 文字搜图 | deepghs/siglip_beta 第一个检查点的文本塔建在冻结的 WD SwinV2 v3 上，作者写明与现有嵌入兼容、不要当成可用于生产（已复核） | 排在组合检索和中文标签名之后 |
| 疑似 AI 提示（#184） | 主流做法仍是「现成特征加小分类器」，没查到能泛化的二次元专用检测器 | 维持原判定标准，待测清单加 PE-Core、DINOv3 |
| 个人偏好排序 | 通用审美打分和个人判断的一致率约 0.58，个人小分类器 0.73–0.81（单人数据） | 有了「收藏」再做，用来排首页和画廊 |

Pixiv 图现在只找回约 6%，[PROVENANCE.md](PROVENANCE.md) 写的是「除非出现新的种子来源」。画风模型是目前唯一查到的新来源，所以排在前面。

### 1.6 后端

- **人工结论导出成一份小文件**：既是自动备份，也是以后换电脑、跨设备同步的基础。建议进 0.1.x。
- **研究任务不再整库复制**：先加分阶段计时，再改成按范围取数。用户感觉到的是「确认之后多久学完」，和 #404 相关。
- **来源可以搬家**：文件夹图片的 ID 绑在「来源加路径」上，搬目录或换电脑就接不上。按内容认回原 ID 的逻辑现在只有抢救在用。
- **提前找下一堵墙**：加 10 万、50 万张的合成规模测试。已知证据包上限是 10 万张（`cc/package.rs` 的 `MAX_ASSETS`），每次同步会逐图重读标签。
- **格式**：文件夹只收 jpg、jpeg、png、webp、gif，等有人反馈再加别的。
- **开发流程**：不需要新方向，编译提速、Windows CI 都已在分派的 Issue 里。

### 1.7 跨设备

第 0 步建议做「手机浏览器访问」：桌面端开一个局域网入口，扫码配对，手机上只逛不改。它比原生 App 便宜得多，前端的 `core/` 层本来就是为换布局留的。之后才是远程算力和原生 App。现有识别结果包绑定原文件的完整内容，「手机传缩略图给桌面算」需要新定一种输入。

### 1.8 不建议做

- 把主模型换成更大的标签模型：PixAI v1.0 输入 1008 像素，没有独显基本跑不动。
- 入库时逐张用小型视觉语言模型写描述：没有独显的机器上，全库要以天计。
- 现在就做原生手机 App，或者换前端框架。
- 推理运行库迁移：#290 可以降到最低优先级。

### 1.9 待用户决定（2026-11-14 已按建议定）

1. 主线顺序，尤其是 0.2 以「找和逛」为主题。
2. 研究解冻的顺序。建议：自建标签接入，新角色覆盖，画风候选，多人图裁人，疑似 AI。
3. 「画廊」是否按「保存的筛选加封面」来做，要不要加「收藏」。
4. 手机浏览器访问做不做。它会让应用第一次监听局域网。
5. 0.1.1 什么时候发。

## 2. 前端现状与缺口

调研时没有运行应用和测试；`ui/settings.js`、`ui/guide.js`、`ui/models.js`、`ui/tasks.js` 和 `app.css` 没有通读，只做了统计和抽查。「脚本统计」是用正则数的。

### 总判断

1. 「整理」做得深，「找图、看图、用图」薄：检索只能单值过滤，没有收藏、保存的筛选、时间轴、画师目录、打开原文件、拖出。
2. 【推断】5 万张时网格本身不是瓶颈。风险在网格之外：每次修改后整份重读元数据，几个面板页全量渲染，缩略图不看屏幕倍率。
3. 【推断】无构建路线站得住。代价有三类：重画靠各处手写的字符串比对，没有静态检查，测试碰不到真实浏览器。没发现非换框架不可的问题。
4. 图库页没有任何原生权限，新的原生能力都要走外壳的本机桥。
5. 文档里没有前端渲染性能的测量，真实规模的接口数字停在 2026-09-28。

### 2.1 结构

- 页面：16 个声明加一个虚拟的角色页（`core/pages.js:46-87`），主区 6 个互斥面板（WEB_UI_MAP › 1）。
- 状态：一个全局可变对象，按组单一写入者（`core/state.js:10-54`）。没有响应式，重画靠显式调用（`core/pages.js:105-112`）。
- 渲染：模板字符串写 `innerHTML`，共 83 处（脚本统计）。只有图片网格做了虚拟滚动。
- 后端交互：`fetch` 加 cookie 和 CSRF 头（`core/session.js:27-44`）。任务状态全靠轮询，进行中 0.7–2 秒、空闲 30 秒一次（`core/jobs.js:87,252`；`ui/tasks.js:130`）。没有 SSE 或 WebSocket。
- 测试：34 个文件约 230 项，Node 导入模块加假 DOM，假元素的 `innerHTML` 只存文本（`tests/web.cjs:35-65`）。测逻辑（元数据合并、轮询、页面表、快捷键表、浮层栈、译文），不测真实 DOM、样式、焦点、拖选和 WebView 差异。仓库里没有浏览器自动化。
- 体积（字节／行）：`ui/settings.js` 76,270／756，`ui/grid.js` 67,424／869，`ui/catalog.js` 51,505／567，`ui/palette.js` 34,110／369，`ui/detail.js` 29,390／276，`ui/shell.js` 27,488／248，`app.css` 135,609／1,498，`locales/en.json` 211,559，`index.html` 13,794／68（最长一行 3,169 字符）。`ui/` 合计 416,417／4,649，全部 JS 553,661／6,519。【推断】按 40 字节一行折算约 1.4 万行，#316 写的「约 4000 行」低估了体量。

### 2.2 大图库下的性能

- 网格：每页 100 张，一次最多读 4 页，上下各预读一屏，节点复用，图片懒加载（`ui/grid.js:28,42-45,266,286-321`）。筛选和搜索都在服务端，搜索框停 250 毫秒后重读第一页（`ui/shell.js:142`）。
- 角色目录：整份读进内存（`core/meta.js:83`）；资料库的作品标签页画出该作品全部角色，列表每次 300 行（`ui/catalog.js:198-203,248-254`）。
- 全量加载或重画：① 每次写入后整份重读计数和角色目录，注释写服务端约 0.6 秒（`core/meta.js:19-24`）；②「相似的图」一次返回并画出全部组，扫描期间每 3 秒整页重画（`nt/http_browse.rs:1261-1318`；`ui/review.js:68-82`）；③ 首页只追加不回收，修改后只重读前 20 天（`ui/home.js:28,46`），【推断】深滚后修改会丢位置，未实测；④ 等学习收尾时反复读整份分组（`core/meta.js:30`）。
- 缩略图：按需生成，长边 400／800／1600／3000（`nt/app_files.rs:96-121`）；选档只看 CSS 像素，大于 260 才用 800（`ui/grid.js:247`）。【推断】2 倍屏默认档位的竖图被放大约 1.2 倍。
- 文档数字：真实规模约 4.8 万张、2026-09-28、Python 读取函数，角色列表 463 毫秒（隐藏 R18 时 1,139），图片第一页 26，按标签 96（TERMS_LABELS › 4）。合成 4.8 万图 8 千角色、三次中位数，角色 NSFW 查询 516，图片 NSFW 查询 25；同节写明此后没在真实副本上验收（TERMS_PERFORMANCE › 1）。前端帧率、首屏、内存没查到。

### 2.3 文档里写明暂缓或未做的

- 画廊视图暂缓；按标签建 Photos 相簿先不做；移动端以后做（BACKLOG › 功能 9、功能 7、移动端与桌面端）。
- 「认一认」的提问频率和卡片内看大图、「可能认错」的展示可改进（BACKLOG › 功能 1、角色身份整理：后续）。
- 拖滚动条跳到未加载深处，行会再排一次（WEB_V2 › 浏览界面）。
- 没做「只看已选」、已检查分隔线、删除副本、同步副本归属（WEB_V2 › 与提示词的偏差、重复图）。
- 英文复数只做了几处，macOS 默认菜单是英文（WEB_V2 › 界面语言）；启动页读不到主题设置（ARCHITECTURE_DIRECTION › 启动页、欢迎页与退出）。
- 位置记忆只在本页面内，提示条不能手动关，触摸屏不能拖选（WEB_UI_MAP › 2、3.2、5）。
- 分组、相似组、身份整理、画师抽查、触摸屏只核对过代码（WEB_UI_MAP › 8）；Windows 没走查（#386 收尾评论）。
- 渲染方式「若仍别扭再评估小型声明式库」（#316 › 暂定方向）。
- WEB_V2 › 新角色流程末尾的「还没做」三项后面已写成做完，这一行过期。

### 2.4 能力缺口

| 能力 | 状态 | 证据 |
|---|---|---|
| 多选与批量 | 有 | 连选上限 5,000、框选、按条件全选（`ui/grid.js:384-434,515-554`） |
| 拖拽 | 没有 | 只有拖动选择；图片 `draggable="false"`（`ui/grid.js:266`），没有 drop 处理 |
| 快捷键 | 有 | 一张表（`core/modes.js:89-114`）；没有自定义入口 |
| 命令面板 | 部分 | 只选角色和标签（`ui/palette.js:33-97`） |
| 搜索语法 | 部分 | 一个词（文件名或角色名），另可各带一个标签、我的标签、画师、作品、某一天；不能多标签、排除、按来源、日期区间（`nt/http_browse.rs:175-176,368-416`） |
| 保存的筛选 | 没有 | 筛选条件已全写在地址里（`core/route.js:20-27`） |
| 收藏、评分 | 没有 | 全前端搜不到 |
| 幻灯片、全屏 | 部分 | 大图是页内覆盖层（`ui/detail.js:216`），无自动播放 |
| 缩放、并排对比 | 缩放有，对比部分 | `ui/detail.js:248-274`；「判断关系」是静态并排（`ui/palette.js:330-347`） |
| 时间线 | 部分 | 首页按加入日期分组（`ui/home.js:21-61`），不能按年月跳转 |
| 统计页 | 没有 | 只有角色月度曲线（`ui/detail.js:43-55`）等零散数字 |
| 深浅色 | 有 | `ui/settings.js:254` |
| 窄窗口、触摸 | 部分 | 16 处 `max-width` 查询；触摸只做了海报翻图（`ui/catalog.js:151-178`） |
| 无障碍 | 部分 | 卡片是没有 `tabindex` 的 `div`（`ui/grid.js:305`），进不了 Tab 序；没查到读屏测试 |
| 导出选中的图 | 没有 | 只有整库导出（`ui/settings.js:369-371`） |
| 打开原文件、访达中显示 | 没有 | 详情只显示文件名（`ui/detail.js:80`） |
| 多窗口 | 没有 | 单主窗口（`desktop/src/main.rs:109`） |

### 2.5 技术路线的代价

1. 「内容没变就不重画、正在输入就不重画」手写了至少 8 处：`ui/settings.js:120-122,484`、`ui/tasks.js:115`、`ui/models.js:128`、`ui/guide.js:141,199`、`ui/dom.js:50-68`、`ui/grid.js:292`；资料库还要手工恢复光标（`ui/catalog.js:549`）。
2. 异步回来要手工判断还在不在这一页，这类守卫约 85 处（脚本统计）。
3. 转义靠调用处自觉：112 处把译文直接写进 HTML 属性，没有检查规则管这件事（`tools/check_frontend.py:232-272`）；抽查没见到漏转义。
4. 没有类型和未定义标识符检查：`setFit` 漏导入，到点击才报错（#386 审查条目 1-9，已修）。
5. 大文件长行让并行开发互相挡路：基线冻结 277 行超长、42 条循环导入边（`tools/frontend_baseline.json`）；#316 写明三项因都改 `grid.js`、`shell.js` 只能按顺序做。

文档给的理由：原生模块在 WebView 和浏览器都能用（#199 › 方向）；前端体量小、Rust 直接发静态文件（#316 › 暂定方向）；`core/` 能在 Node 里直接测，并留给移动端（WEB_V2 › 前端模块）。

【推断】真正吃亏的只有一类：带输入框、又被轮询反复重画的面板。一个不需要构建的局部更新小工具就能解决，不构成换框架的理由。

### 2.6 桌面外壳

- 现在做的：启动宿主和启动页（`desktop/src/server.rs:305`）；一个主窗口（`desktop/src/main.rs:109-113`）；macOS 菜单加后退、前进、刷新（`main.rs:82-108`）；原生选择框（`desktop/src/picker.rs:9-16`）；应用内更新；Pixiv 登录通道（`desktop/src/bridge.rs:54-62`）。图库页没有任何 Tauri 权限（`desktop/capabilities/main.json`）。
- 代码里没有：托盘、系统通知、文件拖入、文件关联、深链、窗口位置记忆、多窗口。Windows 不设菜单（`main.rs:82`）。关主窗口即退出（`main.rs:147-150`）。
- 界面偏好 16 个键存在 `localStorage`，靠固定用 8765 端口保住，端口被占就换（`server.rs:206-213`）。
- 【推断】有价值的：后台运行加完成通知（首次识别 5 万张要数小时，按 TERMS_PERFORMANCE › 6 每张 0.24–1.1 秒推算，而关窗就停）；访达中显示和拖出图片；窗口记忆；把文件夹拖进欢迎页。价值低的：深链、多窗口。

### 2.7 值得做的方向

1. 组合检索加保存的筛选。补上图库类应用最核心的缺口，也给「画廊视图」一个落点。代价：后端过滤要支持多值和排除；我的标签只回前 60 个（`nt/http_read.rs:680`），要一起改。
2. 补「用图」动作：收藏、打开原文件、访达中显示、拖出、幻灯片。代价：本机桥要加路由并限定只认图库内的图；收藏要落数据层。
3. 补浏览维度：按年月跳转、画师目录、统计页。代价：各要一个聚合接口。
4. 写入后改增量刷新，任务状态合成一个变更通道。去掉每次修改后约半秒的整份重读。代价：现有接口回复被逐字节冻结（ARCHITECTURE_DIRECTION › 生产入口与测试参考），【推断】宜加新接口。
5. 网格以外的页面分页，缩略图按屏幕倍率选档。代价：后者只改一处判断，缓存变大。
6. 真实浏览器走查脚本和静态检查进仓库。代价：要无头浏览器和带识别结果的合成数据；类型检查要引入 Node 依赖，与现行做法冲突，需要用户定。
7. 外壳补后台运行、通知、窗口记忆，偏好集中存放。代价：要重新定义关窗和任务收尾的关系。

## 3. 后端、架构与工程现状

文档缩写：ARCH、BT、PERF、LIB、SCHEMA、RP、EP、DEV、LC、INST 依次是 ARCHITECTURE_DIRECTION、BACKEND_TASKS、TERMS_PERFORMANCE、LIBRARIES、TERMS_SCHEMA、RESULT_PACKAGES、EVIDENCE_PACKAGES、DEVELOPMENT、LOCAL_CHECKS、INSTALL。全程只读，没有构建、测试或启动应用，推断均未实测。

### 总判断

- 模块边界和各任务的恢复机制是齐的。弱点是没有通用任务队列、两库不原子，每个后台领域各带一套恢复记录。
- 文档里的测量最大只到约 5 万张。代码里有三处会直接撞墙：研究任务每次整库复制且限时 30 分钟，溯源证据包上限 10 万张，每次同步逐图重读标签。
- 证据包、结果包、任务身份信封可以支撑「别处算、这里导入」。还缺传输与配对、人工结论的可传输格式、跨位置的资产身份。
- 文件夹图片的 ID 绑在「来源 + 路径」上，换电脑或搬目录接不住，会比移动端先遇到。
- 工程侧：界面没有自动化端到端测试，CI 没有 Windows，发布全手动且未公证未签名，用户现场拿不到日志。

### 3.1 架构事实

- 依赖：desktop → 宿主、platform；宿主 → core、platform；platform → core（ARCH › 应用服务与客户端入口）。
- 外壳与宿主：外壳把自身可执行文件作为子进程以 `http --native` 启动（`desktop/src/server.rs:445-468`）。约定是 stdout 的就绪行与进度行、stdin EOF 退出、带口令的 loopback 桥（ARCH › 启动信号与会话 cookie）。
- 宿主与核心：进程内调用，一条人工命令一个事务（ARCH › 目标职责）。
- 宿主与 worker：每个任务一个子进程，参数是 request.json 和 response.json。四种协议是 `illustash-` 前缀的 index/1、sync/2、provenance/1、research/1（`nt/worker_protocol.rs:30-36`）。worker 只写隔离目录，宿主校验后发布。
- 任务：没有通用队列。`Session` 持有同步、名字、溯源、索引、研究各自的协调器（`nt/app_service.rs:123-148`）。公共的 `Task` 提供合作式取消和 idle、limit 两种时限（`nt/app_tasks.rs:84`）。主循环每秒调一次 `recover()`（`nt/app_http.rs:790-808`）。
- 取消在检查点生效。恢复靠各自的持久记录：`backend_effects`、`research_runs`、`sync_runs`、`index-job.json`、`native_delivery`、`source.scan-pending.json`（ARCH › 公共后台任务与会话结果边界、原生角色索引维护；EP › 原生溯源计算交付）。互斥是逐条写死的规则。
- HTTP：约 77 个 `/api/` 路径，读侧约 40、写侧 50，十余个共用。检查统一在 `app_http::handle`，再分到 `http_host_get.rs`、`http_host_post.rs`。
- 数据库，每个图库三个：`library.sqlite`（WAL，应用 schema 36，词条 schema 8；`nt/app_library.rs:16`、`cc/schema.rs:198`）；`source.sqlite`（回滚日志，没有版本号，按列校验；SCHEMA › 13）；`provenance.sqlite`（user_version 5；`nt/app_provenance_cache.rs:21`）。
- 升级只能显式触发：检查两库、backup API 备份、单事务迁移，只接词条 5/6/7→8（ARCH › 已有核心库的显式升级边界）。

### 3.2 性能与规模

| 项 | 数值 | 口径 | 出处 |
|---|---|---|---|
| 开库 | 正常 0.04 秒，异常退出后 55 秒 | 约 10 GiB 合成库，M2，三次中位数 | ARCH › 完整性检查 |
| 升级 / 抢救 | 27 秒 / 158 秒 | 合成库约 6.3 GiB（抢救是 6.23 GiB 的 source），同上 | ARCH › 已有核心库的显式升级边界、source.sqlite 抢救 |
| 核心操作 | 全量 refresh 5.6 秒，刷新 1,000 个名字键 2.5 秒，角色 NSFW 查询 516 毫秒，撤销 40–114 毫秒，确认一张 16 毫秒 | 合成 4.8 万图、8 千角色，三次中位数 | PERF › 1 |
| 识别 | M2：CPU 1,099 毫秒/张，双 Core ML 约 358；Windows：CPU 914，DirectML 236 | 200 张合成图，三次中位数 | PERF › 6 |
| 相似扫描 | 签名 11 分钟，2,718 对比较 35 分钟 | 真实库约 4.7 万张，2026-09-24，当时是 Python 实现 | WEB_V2 › 全库找相似 |
| 体积 | library 约 1.1 GB；source 约 6 GiB | 前者真实库 2026-09-29，后者按约 5 万张造的合成库 | TERMS_CHARACTERS › 7；ARCH › source.sqlite 抢救 |

没查到：原生实现下学习、聚类、相似扫描的单次耗时，图库没变化时一次同步的耗时。第二轮优化后没有在真实副本上测过（PERF › 1）。

成本随图库变大的路径：

- 研究每次运行都用 backup API 把两个库整份复制到系统临时目录，按需的「判断关系」和 OC 分组也一样（`nt/app_research.rs:877-919、929-935`）。
- 学习类另外对两库几乎所有表做全列排序的整表哈希（`nt/app_research.rs:229-270`），发布前最多再算三轮。worker 每次把全部 CCIP 向量读进内存（`nt/research_compute/features.rs:81`）。
- 研究的时限是固定 30 分钟（`nt/app_research.rs:723-728`）。相似扫描单线程逐张签名、逐对比较，结果结束时才一次交回（`nt/research_compute/similar.rs:215-312`），候选配对是全矩阵乘（同文件 58-101）。
- 每次同步的刷新对每张在库图查一次 `labels` 并算哈希，逐图 UPDATE，往 `meta` 写每图两个键，再对全库重算副本组（`nt/app_sync.rs:507-518、578、594、604`）。按 EP › 3 的约 2000 万条估，每图约 400 条标签。
- 溯源证据包的清单列出应用库全部资产（`nt/app_provenance_cache.rs:88-91、157`），而证据包上限是 10 万张（`cc/package.rs:24、381`）。
- 每次 hybrid 决议整体重建身份图，名字任务提交前整读身份与链接（TERMS_NAMES › 8）。

文档没有记「到多少张会出问题」，只有上限：每包 10 万张、单文件 1 GiB（RP › 文件契约），研究回包 128 MiB（`nt/app_research.rs:996`）。

【推断】到 50 万张，按线性外推 source 约 60 GB、library 约 11 GB：

1. 最先撑不住的是研究链路。每次运行要复制约 70 GB，30 分钟内跑不完，中间结果又不保留，相似扫描和学习会一直完不成。约 5 万张的现状离时限多远要实测。
2. 过 10 万张，溯源结果会因包上限交付失败。
3. 每次启动的同步要读约 2 亿条标签。
4. 首次识别在 M2 上约 50 小时。

### 3.3 文档明写未做的后端事项

- 新类型判定器：「只当标签」的正式类型、CP、系统生成的组类型（TERMS › 5 剩余工作；SCHEMA › 6）。
- 名字缓存跨图库共享；`.pilib` 没登记为包类型；从另一个图库导入人工结论；不做实时监听（LIB › 多图库与来源约定、8、0、4）。
- 名字任务的指纹核对、身份图重建仍与规模有关（BACKLOG › 已知的局限）。
- Photos 没有真实相册和权限验收（ARCH › 原生来源同步与应用刷新）。Pixiv 真实账号、真实图片识别质量、大图库都未验收（BT › 验收）。
- 只有核显的 Windows 没测（BT › 识别的执行后端）；干净 Windows 安装和 SmartScreen 没实测（INST › Windows）。
- Unix 上宿主被 SIGKILL 时不保证清理 worker（ARCH › 公共后台任务与会话结果边界）；备份只含 library.sqlite（ARCH › 应用生命周期）。
- iOS/Android、分享、通知没有适配（ARCH › 应用服务与客户端入口）；离线结果包不等于通用远程调度（BACKLOG › 功能 3）。
- 文档与现状不符两处：`docs/DEVELOPMENT.md` 开头仍写发布流程没走过；README 说升级前备份在 `backups/`，代码写到 `library-backups/`（`cp/platform_backup.rs:7`）。

### 3.4 已铺好但没用足的基础

- **识别结果包 v2**：命令行和 `POST /api/recognition/import` 能导入别处算出的包，幂等、可恢复（RP › 使用、事务、去重与后处理）。限制：包要绑定图库 ID、资产 ID 和原文件完整字节的 SHA-256，导入时本机重读原文件核对；界面没有入口（ARCH › 导入任务）；不接收角色认领（RP › 原生计算与内部角色认领）。【推断】「手机传 448 px 缩略图给桌面算」与这套输入绑定和预处理版本不相容，要先定新的输入类型。
- **证据包**：机器证据只增不删，按指纹去重（EP › 3），可直接用来合并别处的机器结果。包里禁止人工来源（EP › 1），人工结论没有任何可传输格式。【推断】`term_verdicts` 是带 `action_id` 的追加表（SCHEMA › 3），适合做成同步日志。
- **资产 ID 与 `asset_links`**：只写入 `photos`、`file` 两种（`nt/app_photos.rs:95`、`nt/app_folder.rs:236`），没有 `md5`，读取只有一处（`nt/app_sync.rs:540`）。文件夹认图限同一来源（`nt/app_folder.rs:84`）。按路径或 md5 认回原 ID 的逻辑只有抢救在用（`nt/app_rescue.rs:197`）。没有修改来源路径的入口，重加只认相同路径（`nt/app_sources.rs:70-74`）。
- **导出归档**：两库同一时刻的快照，清掉密钥（ARCH › 原生图库管理与归档）。导入只能新建图库，不能合并。
- **worker 协议**：身份信封、输入指纹、迟到结果拒收都有，但请求里传的是本机路径和快照目录。宿主只监听 127.0.0.1（`nt/app_http.rs:714`），没有配对和鉴权。
- **多图库**：同时只开一个，文件锁防双开（ARCH › 应用生命周期）。
- **备份与抢救**：备份只有手动和升级前两种（`nt/http_host_post.rs:325`、`cp/platform_backup.rs:7`），没有定期自动备份和保留策略。抢救能保住文件夹图的 ID（ARCH › source.sqlite 抢救）。

### 3.5 来源接入

- 来源：文件夹，以及 macOS 照片。后者直接只读打开 `Photos.sqlite`，只取未删除的图片，优先读派生 JPEG（`cp/platform_photos.rs:16-40`）。
- 触发：启动和切库时自动（`nt/app_service.rs:419、568`），手动，增删来源后（`nt/http_host_post.rs:355、391、412`），模型下载好后页面触发一次（BT › 发布用的小安装包）。不监听文件（LIB › 4），开着的时候不定时重扫。
- 格式：文件夹只收 jpg、jpeg、png、webp、gif（`cp/platform_folder.rs:75`）。解码器另有 tiff、bmp、ico、pnm、tga（`native/terms/Cargo.toml:24`），没有 AVIF、HEIC、JXL。【推断】照片图库里没有派生图的 HEIC 原图解不了，会记为损坏。
- 动图：相似签名和像素哈希只取 GIF 首帧（`nt/research_compute/relation_image.rs:1505`），`/image` 一律输出 JPEG（ARCH › 原生静态资源与图片服务）。
- 视频被排除。多页作品、ugoira、压缩包、漫画没有任何处理，「保存段」类型没实现（TERMS › 5）。

### 3.6 工程基础设施

- 本机全量（`tools/check_all.sh`）：工具自测与结构检查，五个项目的 fmt、clippy、test，Node 测试，release 构建，Python 全套，空 PATH 契约重放。只支持 macOS。
- 耗时：569 秒（2026-10-13，M2，热缓存，单次；LC › 并行运行与耗时口径）。Windows 完整 14 步冷编译 15 分 46 秒，重跑 5 分 59 秒（LC › Windows 验收）。
- CI：仓库已公开，`ci.yml` 对同库非草稿 PR 自动跑五个 job，只作额外保障，没有 Windows。文档还没记耗时（LC › GitHub Actions）；2026-11-14 一次成功运行约 14 分钟（单次）。
- 测试：Rust `#[test]` 约 550 个，Python 64 个文件 684 个用例（经 HTTP 黑盒测宿主），Node 34 个文件，另有固定契约与基准重放。
- 文档承认的薄弱处：固定期望来自已删除的 Python 参考，不能重录（ARCH › 生产入口与测试参考）；对照不覆盖导入建作品（LC › 固定期望与共享夹具）；integrity、rescue 负载高时不稳（DEV › 开发与验收）。仓库里没有界面端到端测试。
- 发布：手动 11 步。更新走 Tauri updater 加 minisign，私钥只在用户手里（DEV › 发布、更新签名密钥）。
- 签名：macOS 是 ad-hoc 签名，没公证；Windows 没签名。手动覆盖安装后要重新放行并重新授权照片，应用内更新不用（INST › 更新到新版本之后）。
- 诊断：只有枚举值和错误编号，128 条、14 天，不含错误原文（DEV › 本地运行诊断）。宿主的 stderr 只在外壳内存里留 30 行，不落盘（`desktop/src/server.rs:526`）。
- Python：`tools/`、`tests/`（约 2.1 万行）、`packaging/build.py` 都依赖 Python 3.12。没查到是否继续退役的表态。

### 3.7 风险点（推断）

1. `nt/app_research.rs`（2,465 行）与非 WAL 的源库。读方靠每 20 毫秒探测 `source.sqlite-journal` 是否存在来让路（:396-455），再叠加三轮指纹和退避重试。最难维护。
2. 两库不原子。扫描、识别、索引、溯源各有一套「一边已提交」的恢复标记（ARCH › 原生来源同步与应用刷新）。最容易出不一致。
3. 人工结论和认回文件夹图 ID 的对照表都只在 `library.sqlite` 里，它没有定期自动备份。
4. Photos 适配依赖 Apple 私有的表结构和派生文件路径（`cp/platform_photos.rs:16、40`），系统升级后可能失效，又没有真实相册验收。
5. 与 NumPy、Pillow 逐位一致的数值和解码合同（`nt/research_compute/relation_image.rs`，1,898 行），绑着不能重录的夹具和一批钉死版本的依赖。升级依赖和改算法都贵。

### 3.8 值得做的方向

1. **研究链路去全量化**：按范围取快照，相似扫描分批发布并改成按进展计时。收益是消掉最先到的规模墙和锁争用，代价是改协议和指纹口径。
2. **源库改 WAL**：去掉探测日志文件的让路机制。代价是迁移，抢救和完整性检查要跟着改。文档没写源库不用 WAL 的原因。
3. **同步刷新增量化**：标签摘要在写入时维护，不再每次逐图重读。收益是启动同步不随图库变慢，代价中等。
4. **溯源交付解除 10 万张上限**，并在计时流程里加 10 万、50 万级的合成规模基准。代价小，能提前发现下一堵墙。
5. **来源可重定位，按内容哈希认回原 ID**：把抢救里的认图逻辑做成常规能力，补写 `md5` 链接。解决换电脑和搬目录，也是跨设备 ID 链接的前置。
6. **人工结论的可传输格式和自动滚动备份**：先做到「从另一个图库导入人工结论」，再谈设备同步。
7. **远程算力先定契约**：为缩略图输入定义新的输入绑定和预处理版本，之后才是局域网传输与配对。
8. **现场可诊断性与发布**：可选的本地脱敏日志，发布脚本化，评估公证与代码签名。

## 4. 同类产品、用户痛点、分发与外部平台

没有注册、下载或安装任何东西。Reddit 无法读取，改用 HN、Lemmy、V2EX、GitHub、日文站代替。

### 总结论

1. **差异点成立**：检索范围内没有产品同时做到「作品→角色→造型目录、纠错后本机学习、未知角色聚类、只发哈希和签名找出处」。最接近的是个位数 star 的个人项目。
2. **壁垒在流程不在模型**：开源打标模型已自带数万角色标签；Eagle 2026-03 上线离线 AI 搜索和 AI 归类，Hydrus 在做外部标签建议接口。
3. **最该补的短板是分发信任**：Windows 未签名包的 SmartScreen 信誉每版清零；macOS 未公证要进系统设置放行，且被 Homebrew 官方库拒收。
4. **最现实的分发**：Windows 上 Microsoft Store（个人免费、商店签名）加 winget；macOS 付 99 美元/年公证，没有免费替代。
5. **第二短板是手机浏览**：Eagle 评价里最常见的缺点是没有移动端，同类小项目已加局域网手机网页。
6. **触达先中文后英文**：V2EX、小众软件、LINUX DO、少数派，再到 Show HN 和 Lemmy；素材一律全年龄。
7. **「只发签名」是真差异也有代价**：只有 Danbooru 自家接口收签名，IQDB.org、SauceNAO、ascii2d 都要传图或给 URL。
8. **移动端先做瘦客户端**：Tauri 2 已有双商店在架应用，但安卓端推理链路成本高。

### 4.1 同类与相邻产品

| 产品 | 定位、平台、许可 | AI 与出处能力 | 二次元适配与抱怨 | 来源 |
|---|---|---|---|---|
| Eagle | 设计素材库；Mac/Win；买断 34.95 美元 2 台 | 离线以图搜图和自然语言搜索插件（2026-03-17）；AI Action 用自选大模型改名、打标、归入**已有**文件夹（2026-03-30）；V5 开发中无日期 | 私有库结构、导出丢层级；无移动端；无角色和出处 | [商店页](https://en.eagle.cool/store)、[博客](https://en.eagle.cool/blog)、[V2EX 2025-09-30](https://www.v2ex.com/t/1162960) |
| Hydrus | 标签式本地库；三平台；开源 | 无内置 AI，靠 API 外接打标器；PTR 按 SHA256 众包画师、角色、作品标签（5400 万文件）；v695 筹备外部标签建议 | 圈内事实标准；界面和安装被反复吐槽 | [版本](https://github.com/hydrusnetwork/hydrus/releases)、[PTR](https://hydrusnetwork.github.io/hydrus/PTR.html) |
| TagStudio、Allusion | 标签式文件管理；GPL-3.0 | 均无 AI；TagStudio 维护者表态不内置模型 | 不搬文件；仍是 Alpha（v9.6.4，2026-10-02） | [版本](https://github.com/TagStudioDev/TagStudio/releases)、[#871](https://github.com/TagStudioDev/TagStudio/issues/871) |
| Billfish、Pixcall、PicSee | 素材管理；免费或本地免费加云付费 | Billfish 有以图搜图；另两者未见 AI 标签 | 面向设计师；Billfish 被指停更（未核实） | [Billfish](https://www.billfish.cn)、[Pixcall](https://pixcall.com) |
| Immich、PhotoPrism、digiKam | 照片库；前两者要自建服务端 | 语义搜索、人脸、去重；digiKam 9 自动标签 | 按真实照片训练，动漫效果未核实 | [Immich 3.2](https://immich.app/blog/v3.2.0-release)、[digiKam 9.1](https://www.digikam.org/news/2026-06-07-9.1.0_release_announcement/) |
| Monbooru 等自建 booru | 单文件加网页；AGPL；146 star | 本地 ONNX 打标（WD14、Camie、PixAI）；配套工具查 booru MD5、IQDB、SauceNAO、PTR 回填标签和来源 | 要会部署 | [仓库](https://github.com/monbooru/monbooru) |
| Tag-AI | Python 桌面；1 star | MD5 查四家 booru，再 IQDB/SauceNAO，再本地视觉模型；路由进**已有**作品/角色文件夹，可撤销 | 最接近「按角色归目录」；仅源码运行 | [仓库](https://github.com/murasaki-dev/tag-ai) |
| AI-Anime-Image-Manager-WD | Windows；MIT；2 star | WD14、中文 CLIP、CCIP 角色识别（2026-10-18）；局域网手机网页 | 角色只落成标签 | [仓库](https://github.com/ASakurafoxA/AI-Anime-Image-Manager-WD) |
| ImoutoRebirth | Windows 服务加 PostgreSQL；18 star | 按文件哈希从 Danbooru 等四站拉标签 | 会移动文件 | [仓库](https://github.com/ImoutoChan/ImoutoRebirth) |

- **按角色/作品建目录**：只有 Tag-AI 和 Eagle AI Action 会归类，且都只放进已有文件夹；Eagle 明说「不适合从零建体系」（[来源](https://en.eagle.cool/blog/post/eagle-plugin-ai-action)）。其余把角色当标签。三级目录、纠错学习、未知角色聚类均未见。
- **找回出处**：成熟做法是 MD5 精确查 booru、以图搜图（要传图）、Hydrus PTR 三种，产出的是 booru 标签和 source 字段。未见声明「只发签名不传图」的产品（推断：这是可宣传的差异）。
- 角色标签已商品化：PixAI Tagger v1.0 含 30,877 个标签，Camie 含 26,968 个角色标签（[PixAI](https://huggingface.co/pixai-labs/pixai-tagger-v1.0)、[Camie](https://huggingface.co/Camais03/camie-tagger)，页面日期未核实）。

### 4.2 目标用户的痛点

| 需求或抱怨 | 来源 |
|---|---|
| 存下来的图不知道画师和出处，只能一张张以图搜图 | [日文博客 2026-01-12](https://new-file.hatenablog.com/entry/2d-image-search)、[HN 2023-07-12](https://news.ycombinator.com/item?id=36692092) |
| 原作被删或转非公开后无从追溯 | [Togetter 2025-09-21](https://togetter.com/li/2605411)、[知恵袋 2017-11-19](https://detail.chiebukuro.yahoo.co.jp/qa/question_detail/q14182257119) |
| 命名规则一变就重复保存，同图多格式 | [中文博客 2025-12-08](https://www.himiku.com/archives/about-my-thing-with-collecting-nijigen-illustrations.html) |
| 手动打标太累，想要本地自动打标 | 同上；V2EX 2025-09-30；TagStudio #871 |
| Hydrus 难用：「janky」「GUI is horrible」、安装复杂 | HN 同上；[Lemmy 约 2026-04](https://scribe.disroot.org/post/8432814) |
| Eagle 是黑盒库，导出丢分类，文件名字符被静默改掉 | V2EX 2025-09-30 |
| 想保留原目录结构、不搬文件 | [V2EX 2026-05-29](https://www.v2ex.com/t/1216425) |
| 十万张级别卡顿、索引慢 | [V2EX 2020-08-18](https://www.v2ex.com/t/699453)；Lemmy 同上 |
| 不信任云端，怕泄露和审核 | [V2EX 2020-03-30](https://v2ex.com/t/657535) |
| 没有手机端 | [Product Hunt 评价](https://www.producthunt.com/products/eagle/reviews?page=2) |

补充：不少人最后只按「来源 + 作者」分文件夹，说明画师目录是默认心智模型（推断）。

### 4.3 分发与信任

**macOS**

- Developer ID 证书须由 Apple Developer Program 账号持有人生成，年费 99 美元；减免只给非营利、教育、政府法人（[Developer ID](https://developer.apple.com/developer-id/)、[减免](https://developer.apple.com/support/fee-waiver)）。
- 放行步骤：macOS 15 起取消右键打开，须到「隐私与安全性」点「仍要打开」；Apple 支持页 2026-05-27 版流程相同（[来源](https://support.apple.com/en-us/102445)）。macOS 27（2026-09-14 发布）未见官方进一步收紧（二手）。V2EX 有用户 2026-07-02 报告未公证应用「被移到废纸篓」，原因未核实。
- Homebrew 官方 cask 库 2026-09-01 起禁用未过 Gatekeeper 的 cask，新提交必须通过，`--no-quarantine` 在移除；官方建议自建 tap（[讨论 #6334](https://github.com/orgs/Homebrew/discussions/6334)、[安全说明](https://docs.brew.sh/Homebrew-Security-and-Supply-Chain)）。

**Windows**（主要依据 [微软签名选项文档 2026-10-29](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options)、[SmartScreen 说明 2026-05-04](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)）

| 办法 | 费用与条件 | SmartScreen |
|---|---|---|
| 不签名 | 免费 | 每个新版本从零积累；Smart App Control 拦截无信誉的未签名文件 |
| Microsoft Store（MSIX） | 个人注册免费（[2025-09-10](https://blogs.windows.com/windowsdeveloper/2025/09/10/free-developer-registration-for-individual-developers-on-microsoft-store/)，近 200 个市场，证件加自拍）；商店重新签名并托管 | 无警告 |
| Store 提交 EXE/MSI | 须自带受信 CA 签名、自托管、静默安装 | 商店内安装无提示 |
| Azure Artifact Signing | 约 9.99 美元/月；个人仅限美国、加拿大 | 信誉慢慢积累 |
| SignPath Foundation | 免费；要求 OSI 许可、无专有组件、已发布、活跃维护、全员 MFA、主页有签名政策、可验证的自动构建、「可验证的声誉」（[条款](https://signpath.org/terms)，草案） | 同 OV |
| Certum 开源证书（云） | 49 欧元；仅自然人，需身份验证和项目网址；每月 5000 次（[商店页](https://shop.certum.eu/open-source-code-signing-on-simplysign.html)） | 同 OV |
| EV 证书 | 400 美元以上/年 | 2024 年起不再免拦 |

- Tauri 不直接产出 MSIX；微软 2026-10-07 有 winapp CLI 打包 Tauri 的指南，写明「商店会替你签名」（[指南](https://learn.microsoft.com/en-us/windows/apps/dev-tools/winapp-cli/guides/tauri)）。商店政策 11.7 禁止包含或展示色情内容（[政策 7.20 版](https://learn.microsoft.com/en-us/windows/apps/publish/store-policies)）；本地 R18 筛选是否触线未核实。
- winget：政策未要求签名，要求静默安装、安装包直接来自发布者地址，过杀软和 URL 信誉扫描后人工审核（[文档](https://learn.microsoft.com/en-us/windows/package-manager/package/repository)）。Scoop Extras：至少 100 star 或 50 fork、英文界面或文档。

**建议排序（推断）**

1. 零预算：Microsoft Store 上架 MSIX，同时提交 winget；下载页写清两个系统的放行步骤。
2. 99 美元/年：Apple 公证，Mac 端没有别的路。
3. 49 欧元：Certum 给商店外安装包签名，信誉可跨版本累积，但首批下载仍会弹窗。
4. 有发布记录和用户后申请 SignPath。
5. 满 100 star 后提 Scoop；Azure 和 EV 现在不用考虑。

### 4.4 触达渠道

| 渠道 | 规则与注意 | 来源 |
|---|---|---|
| V2EX 分享创造 | 只写「发布自己的最新作品」；回复 0 到 36 不等；类 Eagle 的 Momento 发帖换来一份详细缺陷清单 | [节点](https://www.v2ex.com/go/create) |
| 小众软件发现频道 | 标题惯例【开源自荐】；两天 15 篇自荐；发了别删帖；置顶规则正文未取到 | [频道](https://meta.appinn.net/c/faxian/10) |
| LINUX DO | 「开源推广」标签：须完整开源并在项目里链接认可该社区；每号每周约 1 帖；不得贴 README、不得 AI 代写、禁色情内容 | [公告](https://linux.do/t/1776670)、[FAQ](https://linux.do/faq)（搜索摘要，原帖未直读） |
| 少数派 | 申请 Matrix 写作权限投「开发者说」；不得假扮第三方；两周无推荐即未通过 | [规则](https://sspai.com/post/40262) |
| Show HN | 必须能直接试用、免注册；作者在场答疑 | [规则](https://news.ycombinator.com/showhn.html) |
| Lemmy、Reddit | Blombooru 作者跨发 opensource 和 datahoarder 社区获得讨论；Reddit 各版规未核实 | Lemmy 同上 |
| 视频 | TagStudio 靠作者一支讲动机的视频起量，播放量未核实 | [视频](https://www.youtube.com/watch?v=wTQeMkYRMcw) |

R18 相关的坑：

- GitHub 禁止色情内容和性行为的图像描绘（含绘画），艺术语境可例外（[政策](https://docs.github.com/en/site-policy/acceptable-use-policies/github-sexually-obscene-content)）。README 和演示库用全年龄图。
- App Store 要求 NSFW 默认隐藏；2016 年有 Reddit 客户端因显眼的 NSFW 开关被下架（二手）。Google Play 2026-04-09 以「敏感主题」下架《心跳文学部》（[报道](https://www.pocketgamer.biz/google-removes-doki-doki-literature-club-from-play-store-over-policy-violation)）。
- B 站、知乎、少数派的审核口径未核实；推断：标题和配图避开「涩图」「R18」，只写「分级筛选」。

### 4.5 外部平台现状

| 平台 | 现状 | 来源 |
|---|---|---|
| Danbooru | 读请求全局 10 次/秒，长任务建议约 1 次/秒；UA 要带用户 ID、不得伪装浏览器，坏 bot 会被封；`iqdb_queries` 接受 `hash`、`url`、`post_id` 等参数。匿名能否调 IQDB 未核实（本项目 2026-09-29 实测匿名可查，见 [PROVENANCE.md](PROVENANCE.md) 第 9 节） | [help:api](https://danbooru.donmai.us/wiki_pages/help:api)、[源码](https://github.com/danbooru/danbooru/blob/master/app/controllers/iqdb_queries_controller.rb) |
| IQDB.org | 在线，搜 8 个站；上限 8192 KB、7500×7500；无 API 文档，只收文件或 URL | [首页](https://iqdb.org/) |
| SauceNAO | 官方页抓取被拒。第三方库显示免费账号约 4 次/30 秒、100 次/日（未核实）；只收图或 URL | [封装库](https://pypi.org/project/saucenao-api/2.4.0/) |
| ascii2d | 在线，无 API，只收图或 URL | [首页](https://ascii2d.net/) |
| Pixiv | 2026-06-22 改规约：禁止超常负载和机械化批量操作；商标指南（2025-01-28 修订，非这次新改）禁止用程序收集作品，复核见 #442；第三方库早已不能密码登录，只能用浏览器取 refresh token。未查到 2025–2026 年新的封禁动作（未核实） | [公告](https://www.pixiv.net/info.php?id=13794)、[规约](https://policies.pixiv.net/index.html)、[pixivpy](https://github.com/upbit/pixivpy) |
| X 归档 | 未查到官方格式变更；仍是 `window.YTD.*` 包裹的 JSON，`like.js` 只有推文 ID、正文、链接（第三方脚本）。2025 年底起大量「申请页转圈、下载不完整」的报告，等待 24–48 小时 | [note 2026-03-22](https://note.com/xbackup/n/n2b670c743c12) |

### 4.6 移动端技术现状

- **Tauri**：2.12 于 2026-09-26 发布（安卓目标 SDK 37、Gradle 至少 8.13，修了 Xcode 27 链接问题）；3.0 在 alpha.6（2026-11-06）（[博客](https://tauri.app/blog/)、[版本](https://github.com/tauri-apps/tauri/releases)）。实际上架案例：Readest（Tauri v2，iOS、安卓、桌面、网页，App Store 和 Google Play 在架，2.6 万 star）（[仓库](https://github.com/readest/readest)）。限制：部分桌面插件没有移动版、仍要 Xcode 和 Android Studio、安卓 WebView 性能因机型而异（二手博客）。
- **安卓跑 ONNX**：NNAPI 在 Android 15 弃用（[官方](https://developer.android.com/ndk/guides/neuralnetworks)），ONNX Runtime 不再给它加算子（[issue 22346](https://github.com/microsoft/onnxruntime/issues/22346)）。QNN 仅限骁龙，NPU 后端只跑量化模型、不支持动态形状（[文档](https://onnxruntime.ai/docs/execution-providers/QNN-ExecutionProvider.html)）；安卓 AAR 最新 1.27.0（2026-06-30，Maven 索引摘要）。LiteRT 的 NPU 加速（高通、联发科）2026-01-28 进入正式栈，但要转成 .tflite，官方未提 ONNX 导入（[Google 博客](https://developers.googleblog.com/litert-the-universal-framework-for-on-device-ai/)）。
- 推断：「手机借桌面算力」避开了以上碎片化；第一步可以是桌面端提供局域网网页，再考虑 Tauri 壳。

## 5. 二次元图像模型与算法进展

内容来自 2026-11-14 访问的模型卡、仓库和论文页，没有下载权重，没有实测。指标多为作者自测，口径不一，不能直接横向比。页面经工具摘要读取，个别数字可能有转述误差，采用前对照原卡复核。方括号数字对应本节末的来源。

### 总结论

最值得马上试：

1. **在已存的 WD 嵌入上加「新角色线性头」，定期用公开 Danbooru 数据重训。** PixAI v0.9 就是冻结编码器只训头，自测角色 micro-F1 0.865 [6]；图库端不用重跑图。
2. **CCIP 换成同仓库的 caformer_b36-24，多人图先检测再裁人。** F1 0.941 对默认款 0.917，聚类得分 0.957 对 0.922 [17]。
3. **文字搜图分两步。** 先做「中日文别名查标签，再查已存标签概率」，不需要模型 [26]；再试 deepghs/siglip_beta 的文本塔，它对齐的就是 WD SwinV2 v3 嵌入 [22]。
4. **审美用个人偏好线性头，不用通用打分器。** 公开打分器和单个用户判断的一致率约 0.58，个人头 0.73–0.81 [40]。
5. **AI 图检测的待测清单加上 PE-Core 和 DINOv3 的冻结特征线性探针** [42]。

不值得做：

- 把主模型换成 PixAI Tagger v1.0：1008×1008 输入、4.86 亿参数，无独显基本不可用，只适合做可选的二次识别 [3][4]。
- 入库时逐张跑小型视觉语言模型写描述（5.8 节）。
- 随包分发 SSCD、ISC21 权重：权重没有许可声明，训练数据只限非商用研究 [33]。
- jina-clip-v2、MetaCLIP 2、Slopscan-Anime、aesthetic-shadow-v2：都是 CC BY-NC [25][41][44]。

### 5.1 比 WD v3 新的标签模型

| 模型 | 发布或更新 | 数据截止 | 角色标签 | 许可 | ONNX 与体积 | 输入 | 公开指标 |
|---|---|---|---|---|---|---|---|
| WD v3（SwinV2、ViT-L、EVA02-L）[1][2] | 2024-03 至 2024-07 | 2024-02-28（帖子 ID 722 万） | 约 2,700 | Apache-2.0 | 官方；SwinV2 467 MB | 448 | F1（P=R）：SwinV2 0.454，EVA02-L 0.477 |
| PixAI Tagger v1.0 [3][4][5] | 2026-09-15 | 2026-05 | 8,308，另有 4,917 个风格标签 | Apache-2.0，不设门槛 | 只有社区导出：fp32 1.89 GB，fp16 947 MB，int8 523 MB | 1008 | 角色 micro-F1 0.894（全词表），0.924（共享标签） |
| PixAI Tagger v0.9 [6][7] | 2025-08（2025-04 训练） | 2025-01（ID 860 万） | 未分列，总词表约 1.35 万 | Apache-2.0，下载需登录并同意条款 | deepghs 导出 1.27 GB | 448 | 角色 micro-F1 0.865（自测），0.820（v1.0 卡复测） |
| wd-eva02-tagger-2026-canary（第三方）[8] | 2026-07-29 | 2026-05-18（ID 1,140 万） | 在 WD 基础上新增 2,205 | Apache-2.0 | 无 ONNX，safetensors 1.28 GB | 448 | F1（P=R）0.542 |
| animetimm dbv4 系列，30 多个主干 [9][10] | 2025-05 至 2026-08 | 未写明 | 3,247 | GPL-3.0，下载需登录（自动通过） | 有；SwinV2-B 470 MB | 384 至 560 | 角色 micro-F1：SwinV2-B 0.927，RepViT（3,040 万参数）0.864 |
| Camie Tagger v2 [11] | 2025-08-31 | Danbooru 2024 数据集 | 26,968，另有 7,007 个画师 | GPL-3.0 | 官方 789 MB | 512 | 角色 micro-F1 0.834（自测），0.726（PixAI 卡复测 [3]） |
| cl_tagger 1.02 [12] | 2025-09 | 未核实 | 16,210 | Apache-2.0 | 官方 1.43 GB | 未核实 | 无自报；通用标签 micro-F1 0.574，低于 WD（第三方 [13]） |
| Kagami-24k、OppaiOracle、JoyTag [13][14][15] | 2026-08、未核实、2023 | — | 都不含角色标签 | Apache-2.0 | 有 | 448 | Kagami 在 ID 大于 860 万的新图上通用 micro-F1 0.671，WD 为 0.637–0.638 |

- SmilingWolf 没有发布 v4 模型，只在 2025-04、2025-05 发布了 v4 数据集（ID 上限约 872 万）[1][2]。animetimm 训练集的划分规模与它一致，【推断】数据截止约在 2025 年初。
- 冻结主干只训头有公开先例：PixAI v0.9 冻结 WD EVA02 编码器 [6]（汇总者复核：模型卡写明「encoder frozen」，同口径下 WD v3 SwinV2 角色 F1 约 0.608）。同一份共享标签测试（2026-06 至 2026-08 的图）里，它的角色 micro-F1 是 0.820，全量训练的 v1.0 和 animetimm 各主干是 0.907–0.927 [3]。【推断】冻结头大约低 10 个点，换来的是零额外推理；5,000 个角色的 1024 维线性头约 20 MB。它用的是 EVA02 特征，SwinV2 上的效果要自己测。
- canary 用 ID 大于 722 万的图做增量训练，是否冻结没写 [8]。没查到按月或按季度持续发布的公开项目，PixAI 大约一年一版。
- 公开训练数据：danbooru2026 数据集，1,100 万张以上，最大 ID 1,174 万，3.73 TB，标注为进行中，是否需登录未核实 [16]。
- PixAI v1.0 的速度：H100 上单张 25 ms [3]；社区实测（Ryzen 5 9600X 加 RX 580 4 GB，DirectML）约 5.3 秒一张，fp32 加载时内存峰值 9.4 GB [4]。社区另有 672、448、336 的低分辨率微调版，336 版在 RTX 5070 上 16 ms [5]。主干是微调过的 SAM3，SAM3 在 HF 上的许可标为 other，与模型卡的 Apache-2.0 是否一致未核实。
- 【推断】如果愿意重算全库嵌入，animetimm 的 SwinV2-B 与现用模型同构、成本相同，角色多约 500 个、数据新约一年，收益有限。

### 5.2 角色身份特征

- CCIP 没有后继模型，deepghs/ccip_onnx 最后更新是 2024-09-09。同仓库里有更大的一款，许可 OpenRAIL [17]。#456 实测两款特征均为 768 维，模型切换后要重算参考和图库向量。下表为作者自测：

| 款式 | F1 | 聚类得分（OPTICS） | ONNX |
|---|---|---|---|
| ccip-caformer-24-randaug-pruned（现用，imgutils 默认） | 0.917 | 0.922 | 150 MB |
| ccip-caformer_b36-24 | 0.941 | 0.957 | 384 MB |

- 检测模型 [19]：deepghs 的二次元检测都是 YOLO 系，带 ONNX，仓库标 MIT，2024-10 之后没有更新。人物检测最好 F1 0.87；头部检测 v2.0 用 YOLOv11，mAP50 0.945–0.969，最小款 259 万参数；脸部检测 v1.4 的 F1 0.94–0.95。
- 先裁人再算特征：imgutils 文档写明 CCIP 只适用于单人图 [18]。社区项目 ccip_grounding 的做法是检测出每个人、裁出来（可只取头部）、逐个和参考图比 [20]。【推断】可行，代价是多人图多跑一次检测和若干次 CCIP。
- [#456 的独立评估](https://github.com/shio-vinyl/illustash/pull/735)支持保留整图 CCIP 和现有长尾锚点。公开评测 515 张，已发布长尾角色的 50 个图与角色配对，整图命中 9 个，加入人体框或头部框各命中 11 个，各多出 1 个标注外候选。有人数标签的多人图里，18 个目标分别命中 0、2、1 个。用同一批参考重做裁图锚点和比较 b36 后，多人收益仍小。现用与 b36 在 M2 的单次整图编码中位数为 47.7 和 103.6 毫秒；产品继续用整图做学习、聚类和相似检索，WD 扩展头的裁图重训在 #551 独立评估。
- 不同造型、Q 版：没查到专门的公开模型或论文。可用来评测的公开数据是 deepghs/game_character_skins，10 个游戏约 2,477 个角色的各套皮肤，含 Q 版立绘，CC-BY-4.0 [21]。ICMR 2026 有一篇 Anime-2026 角色数据集论文，规模和下载方式未核实。

### 5.3 本地文字搜图

| 方案 | 文本语言 | 体积 | 许可 | 要重跑图吗 |
|---|---|---|---|---|
| 别名表查标签，再查已存标签概率 [26] | 中、日、英 | 约 10 MB 的表 | MIT | 否 |
| 小型多语言文本嵌入检索标签百科 [26][27] | 100 种以上 | multilingual-e5-small，1.18 亿参数，带 ONNX | MIT | 否 |
| deepghs/siglip_beta 文本塔 [22] | 英文标签式文本 | 体积未核实，8,570 万参数 | Apache-2.0 | 否 |
| SigLIP 2 base-patch16-256 [23] | 多语言 | 视觉 372 MB（int8 95 MB），文本 1,129 MB（int8 283 MB） | Apache-2.0 | 是 |
| PE-Core-B16-224 [24] | 未说明多语言，文本上限 32 token | 视觉 0.9 亿、文本 3.1 亿参数，HF 上没查到 ONNX | Apache-2.0 | 是 |
| jina-clip-v2、MetaCLIP 2 [25] | 多语言 | 9 亿、6 亿参数 | CC BY-NC | 排除 |

- 别名表：ame-la/danbooru-tags-data-zh 约 10 万条中文译名（角色 35,384，画师 24,881，通用 30,693），计数截至 2026-08，带日文和英文别名。patvessel/danbooru-rag-v3 是清洗过的 9.86 万条标签百科，CC-BY-SA-4.0 [26]。社区已有用 multilingual-e5 做日文、韩文查标签的项目 [27]。
- siglip_beta（汇总者复核）：第一个检查点「在冻结的 SmilingWolf/wd-swinv2-tagger-v3 上训练文本编码器」，写明与 DeepGHS 现有的索引和嵌入兼容；同时写明「不要把这个仓库里的任何东西当成可用于生产」；示例都是标签式文本，没有公布指标 [22]。【推断】应能直接用已存嵌入，要先验证数值一致。WD 作者自己的演示空间也是同类做法 [22]。
- 没查到 2025–2026 年在 Danbooru 上大规模微调、文本侧支持中日文的公开 CLIP 或 SigLIP。SigLIP 2 在二次元图上的中日文检索质量没有公开评测。
- 【推断】笔记本可行性：前三种只在查询时跑一次文本模型，几十万条向量的暴力检索在百毫秒以内，可行。SigLIP 2 base 视觉塔的计算量不到 WD SwinV2 的一半，5 万张在 CPU 上要后台跑几小时，适合做可选功能。

### 5.4 画师与画风

| 模型 | 日期 | 规模 | 指标 | 许可 | ONNX |
|---|---|---|---|---|---|
| Kaloscope 2.0 [28] | 2025-11 | 39,260 位画师；Danbooru 截至 2025-09，每人 40 张以上 | top-1 90.13%（10% 验证集） | Apache-2.0；模型卡另注「仅供研究和教育用途」 | 社区导出 700 MB，448 输入 |
| Kaloscope 1.1 [28] | 2025-10 | 31,770 位 | top-1 85.6% | Apache-2.0 | 社区导出 480 MB，224 输入 |
| PixAI v1.0 风格标签 [3] | 2026-09 | 4,917 个 | micro-F1 0.901 | Apache-2.0 | 见 5.1 |
| Camie v2 画师标签 [11] | 2025-08 | 7,007 位 | micro-F1 0.700 | GPL-3.0 | 官方 |
| CSD ViT-L [29] | 2024 | LAION-Styles，非二次元专用 | 仓库注明权重与论文数字有出入 | 代码 MIT，权重未单独声明 | 无 |
| anime-style-art-embedding [30] | 未核实 | EVA02-B，448 输入 | R@1 0.732 | Apache-2.0 | 无 |
| Anime_Images_Style_Embedder V4 [30] | 2025-10-17 | DINOv3 的 CLS 加小型 MLP | 无 | 受 DINOv3 许可约束 | 无 |

- 猜画师候选（Kaloscope 部分经汇总者复核）：只认训练集里的画师，验证集是同分布的 10%，没有另外的留出测试集；模型卡写明分数是类别间的相对概率，「不代表对图片真实作者的鉴定」；架构 LSNet，约 1.83 亿参数 [28]。【推断】适合当「候选画师」，和关注列表、溯源结果并列展示，再由现有的本地比对确认，不能当结论。
- 把同一画师聚到一起需要嵌入，不是分类概率。Kaloscope 分类头前有 2048 维特征 [28]，【推断】可以重新导出输出该特征的 ONNX，效果没有公开数据。deepghs 发布过画风对比学习数据集 CSIP，没查到对应模型 [31]。

### 5.5 近重复与编辑检测

| 方法 | 许可 | 形态 | 公开结论 |
|---|---|---|---|
| SSCD（ResNet50，512 维）[32] | 代码 MIT；权重无许可声明，训练数据 DISC21 只限非商用研究 [33] | TorchScript，无 ONNX；仓库 2023-10 归档 | 余弦大于 0.75 时约 90% 精度 |
| ISC21 第一名描述子（256 维）[32] | 同上 | PyTorch | 2026-10 的基准里这一类最高检出 75.5%（配对误报率 10⁻⁷）[33] |
| DINOv2 S、B 的 CLS [33] | Apache-2.0 | 有 ONNX | 同一基准 65.0%，许可宽松的方法里最好 |
| DINOv3 [34] | Meta 自定许可，下载需登录并同意 | 社区 ONNX | 没查到拷贝检测数字 |
| RDCD（WACV 2025）[35] | 未核实 | EfficientNet-B0 等小模型，64 至 256 维 | µAP 比基线高约 5 个点 |
| XFeat 局部特征（CVPR 2024）[35] | Apache-2.0 | 无官方 ONNX | CPU 上 VGA 分辨率实时 |
| LPIPS 聚类（imgutils）[36] | 未核实 | ONNX | 推荐阈值 0.45 |

- 能补上的：大幅裁剪、叠字、调色、拼贴正是拷贝检测描述子的训练目标。【推断】用 DINOv2-S 做候选召回，关系仍由现有的像素比较和互相关来定，是许可上最干净的组合。
- 重叠的长图拼接：【推断】只有局部重叠时全局描述子不可靠，要用局部特征加几何验证，或把现有互相关改成条带搜索。同一基准报告几何验证没有带来检出提升 [33]，需要自己测。
- 重绘（像素不同、构图相同）不属于拷贝检测，【推断】靠已有的 WD 嵌入、CCIP 和标签更合适。
- 没查到针对表情差分、文字差分的公开模型或数据集。基准里的误匹配主要来自同一商品的不同变体 [33]，【推断】差分组对全局描述子是同类难题，仍要靠逐对像素比较来分。

### 5.6 审美与质量打分

| 模型 | 日期 | 结构 | 许可 | 备注 |
|---|---|---|---|---|
| waifu-scorer-v3 [37] | 2024-04 | CLIP 特征加 MLP，280 万参数，0 到 10 分 | 卡内写 Apache-2.0，标签写 openrail | CLIP 型号未写 |
| waifu-scorer-moas [37] | 2026-11-03 | CLIP ViT-L/14，全图加四个局部视图 | 打分头许可未指定 | 验证集 SRCC 0.711 |
| deepghs/anime_aesthetic [38] | 2024-03 | SwinV2，448 输入，7 档 | OpenRAIL | 准确率 40.9%，AUC 0.821 |
| danbooru-aes-cls [39] | 2025-11 | WD SwinV2 v3 池化特征上的 4 档分类头 | MIT | 约 2 万样本，无指标 |
| anime-aesthetic-nafcsd [40] | 未核实 | SigLIP 2 加 CSD 特征上的线性头 | MIT | 单个标注者，3,695 张图 |
| aesthetic-shadow-v2 [41] | — | 11 亿参数 ViT（第三方说法） | CC BY-NC（第三方说法） | 原仓库现在返回 401 |

- 关键数据来自 [40]：同一批成对判断上，deepghs anime_aesthetic 准确率 0.578，waifu-scorer-v3 0.585，接近随机；用约 1,500 对个人判断训练的线性头 0.765。
- 【推断】「挑代表图」适合做成个人偏好头：用用户的收藏、评分当标签，在已存的 WD 嵌入上训练，和现有逻辑回归是同一套做法。danbooru-aes-cls 可以直接接在已存嵌入上做冷启动 [39]。
- 【推断】「重复组里选最好的一张」不该用审美分：组内内容相同，分差很小。分辨率、压缩痕迹、有无水印和打码更直接；[40] 的水印线性探针 AUC 0.969，可参考。

### 5.7 AI 生成图检测

- 2026 年的主线是冻结的视觉基础模型加线性分类器。2026-02 的论文报告 Perception Encoder、MetaCLIP 2、DINOv3 的线性探针在真实场景数据上比专用检测器准确率高 30 个点以上，但遇到转存压缩、VAE 重建和局部编辑仍会失效。2026-06 的 SSAFE 只用 1 万张精选训练图加一层线性层。ECCV 2026 一篇用闭式高斯判别头，在 39 个数据集约 710 万张图上与已发布的检测头相当，并指出结果强烈依赖训练集构成。2026-05 一篇指出只要知道主干就能构造对抗样本 [42]。
- 二次元专用的工作：AnimeDL-2M（2025，200 万张以上，含局部重绘）；AniPrO（2026-09，区分手绘、局部重绘、文生图，代码和数据尚未发布）；OmniAID（ICML 2026）的 Mirage 基准单列二次元类别，理由是二次元与照片之间泛化差距明显 [43]。
- 新出的现成模型：Slopscan-Anime（2026-09，ConvNeXt V2，同来源留出集 AUROC 0.9998，没有跨生成器数据，CC BY-NC）[44]。没查到公开证明能泛化到新生成器的二次元检测器，也没查到专门做「二次元加少样本适应」的公开工作。
- 【推断】#184 的待测清单建议加 PE-Core（Apache-2.0，B/16 视觉塔 0.9 亿参数 [24]）；正样本可考虑公开的 AI 图站数据集，如 deepghs/aibooru_full（许可标为 other，卡片没有说明内容，未核实）[45]；评测时把转存压缩后的图单列。

### 5.8 小型视觉语言模型

| 模型 | 体积 | 许可 | 备注 |
|---|---|---|---|
| Florence-2-base [46] | 2.3 亿参数；ONNX int8 合计约 275 MB | MIT | 只出英文描述 |
| SmolVLM2 256M、500M、2.2B [46] | 2.2B 版视频推理需 5.2 GB 显存 | Apache-2.0 | 只支持英文 |
| Qwen3-VL-2B [46] | CPU 用 ONNX 包合计约 3.65 GB | Apache-2.0 | 多语言 |
| Gemma 3n E2B [46] | 等效 20 亿参数，每图 256 token | Gemma 条款，下载需同意 | 140 种语言 |
| ToriiGate v0.4-2B [46] | 20 亿参数 | Apache-2.0 | 二次元专用，90 万张作品微调，可用标签作提示 |
| Moondream 3 preview [46] | 90 亿参数，激活 20 亿 | BSL 1.1 | 不适合笔记本 |

- 公开的速度数字很少：SmolVLM-256M 在 M4 Max 上经 WebGPU 解码约 80 token/秒（论文）；其余只有第三方估计，未核实。
- 【推断】单张按需问答在有独显或 Apple 芯片的机器上现实。无独显的笔记本上，20 亿参数级模型一张图要十几秒到几十秒，5 万张全量生成描述要以天计，不现实。通用小模型基本不认识二次元角色名，仍要靠标签。

### 5.9 推理运行库近况

- ONNX Runtime：1.30.0（2026-09-10）、1.31.0（2026-10-09）、1.32.0（2026-11-13）[47]。
- Core ML：1.31.0 起可以通过设备枚举和选择策略来选 Core ML，偏好 NPU 时选 Apple 神经引擎（需 macOS 13），GPU 需 macOS 12。1.32.0 是最后一个 CocoaPods 版本 [47]。
- DirectML：仍在维护，1.31.0 和 1.32.0 都有修复和新算子 [47]。微软文档把它标为 legacy，新功能转到 Windows ML [48]。
- Windows ML：当前正式版 2.5.96（2026-10-26）内置 ORT 1.28.3，自带的只有 CPU 和 DirectML；AMD GPU、NvTensorRtRtx、OpenVINO、QNN、VitisAI 要在 Windows 11 24H2 以上按需下载；MIGraphX 已弃用 [48]。
- WebGPU：原生版改成了运行时注册的插件库，Windows 走 D3D12 或 Vulkan，macOS 走 Metal，要求 ORT 1.24.4 以上；插件 0.5.0 对应 ORT 1.31.0，0.6.0 对应 1.32.0 [47][49]。在 Windows ML 里仍标为实验性 [48]。
- CPU：1.30.0 起，FP16 的 Gemm 和 MatMul 在没有硬件加速时回退到 FP32 [47]。社区实测 fp16 模型在 CPU 上比 fp32 慢 [4]。
- Rust：ort crate 最新是 2.0.0-rc.15（2026-10-19），对应 ORT 1.31，仍没有 2.0 正式版 [50]。
- 【推断】对现有选择的影响：不需要迁移。DirectML 仍是 Windows 10 和旧版 Windows 11 上唯一的通用 GPU 路径；WebGPU 插件是值得跟踪的跨平台 GPU 候选；CPU 上继续用 fp32。

### 5.10 来源（均于 2026-11-14 访问）

- [1] https://huggingface.co/SmilingWolf
- [2] https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3 ；https://huggingface.co/SmilingWolf/wd-eva02-large-tagger-v3 ；https://huggingface.co/datasets/SmilingWolf/wdtagger-v4-seed
- [3] https://huggingface.co/pixai-labs/pixai-tagger-v1.0
- [4] https://huggingface.co/Mexes/pixai-tagger-v1.0-onnx-fp32-fp16-int8
- [5] https://huggingface.co/HentaiSL/pixai-tagger-v1.0-onnx-1008-672-336-ft
- [6] https://huggingface.co/pixai-labs/pixai-tagger-v0.9
- [7] https://huggingface.co/deepghs/pixai-tagger-v0.9-onnx
- [8] https://huggingface.co/ashen-sensored/wd-eva02-tagger-2026-canary
- [9] https://huggingface.co/animetimm
- [10] https://huggingface.co/animetimm/swinv2_base_window8_256.dbv4-full ；https://huggingface.co/animetimm/repvit_m2_3.dbv4-full
- [11] https://huggingface.co/Camais03/camie-tagger-v2
- [12] https://huggingface.co/cella110n/cl_tagger
- [13] https://huggingface.co/Redstonexs/kagami-24k ；https://huggingface.co/Redstonexs/danbooru-tagger-v1
- [14] https://huggingface.co/Grio43/OppaiOracle
- [15] https://github.com/fpgaminer/joytag
- [16] https://huggingface.co/datasets/nyanko-devs/danbooru2026
- [17] https://huggingface.co/deepghs/ccip_onnx
- [18] https://dghs-imgutils.deepghs.org/main/api_doc/metrics/ccip.html
- [19] https://huggingface.co/deepghs/anime_person_detection ；https://huggingface.co/deepghs/anime_head_detection ；https://huggingface.co/deepghs/anime_face_detection
- [20] https://github.com/quarterturn/ccip_grounding
- [21] https://huggingface.co/datasets/deepghs/game_character_skins
- [22] https://huggingface.co/deepghs/siglip_beta ；https://huggingface.co/spaces/SmilingWolf/danbooru2022_embeddings_playground
- [23] https://huggingface.co/google/siglip2-base-patch16-256 ；https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX
- [24] https://huggingface.co/facebook/PE-Core-B16-224
- [25] https://huggingface.co/jinaai/jina-clip-v2 ；https://huggingface.co/facebook/metaclip-2-worldwide-b16
- [26] https://huggingface.co/datasets/ame-la/danbooru-tags-data-zh ；https://huggingface.co/datasets/patvessel/danbooru-rag-v3
- [27] https://huggingface.co/intfloat/multilingual-e5-small ；https://github.com/joykst96/danbooru-tag-rag
- [28] https://huggingface.co/heathcliff01/Kaloscope2.0 ；https://huggingface.co/heathcliff01/Kaloscope ；https://huggingface.co/DraconicDragon/Kaloscope-onnx
- [29] https://github.com/learn2phoenix/CSD
- [30] https://huggingface.co/dudcjs2779/anime-style-art-embedding ；https://huggingface.co/Fgdfgfthgr/Anime_Images_Style_Embedder
- [31] https://huggingface.co/datasets/deepghs/csip
- [32] https://github.com/facebookresearch/sscd-copy-detection ；https://github.com/lyakaap/ISC21-Descriptor-Track-1st
- [33] https://arxiv.org/abs/2610.04151
- [34] https://huggingface.co/facebook/dinov3-vits16-pretrain-lvd1689m
- [35] https://arxiv.org/abs/2405.17928 ；https://github.com/verlab/accelerated_features
- [36] https://dghs-imgutils.deepghs.org/main/api_doc/metrics/lpips.html
- [37] https://huggingface.co/Eugeoter/waifu-scorer-v3 ；https://huggingface.co/Eugeoter/waifu-scorer-moas
- [38] https://huggingface.co/deepghs/anime_aesthetic
- [39] https://huggingface.co/aipracticecafe/danbooru-aes-cls
- [40] https://huggingface.co/ricecake/anime-aesthetic-nafcsd
- [41] https://huggingface.co/shadowlilac/aesthetic-shadow-v2 （返回 401）
- [42] https://arxiv.org/abs/2602.01738 ；https://arxiv.org/abs/2606.08634 ；https://arxiv.org/abs/2608.18523 ；https://arxiv.org/abs/2605.13381
- [43] https://arxiv.org/abs/2504.11015 ；https://arxiv.org/abs/2609.23345 ；https://arxiv.org/abs/2511.08423
- [44] https://huggingface.co/Dominik72/Slopscan-Anime
- [45] https://huggingface.co/datasets/deepghs/aibooru_full
- [46] https://huggingface.co/onnx-community/Florence-2-base-ft ；https://huggingface.co/HuggingFaceTB/SmolVLM2-2.2B-Instruct ；https://arxiv.org/abs/2504.05299 ；https://huggingface.co/Qwen/Qwen3-VL-2B-Instruct ；https://huggingface.co/google/gemma-3n-E2B-it ；https://huggingface.co/Minthy/ToriiGate-v0.4-2B ；https://huggingface.co/moondream/moondream3-preview
- [47] https://github.com/microsoft/onnxruntime/releases
- [48] https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/onnx-versions ；https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/supported-execution-providers
- [49] https://onnxruntime.ai/docs/execution-providers/WebGPU-ExecutionProvider.html
- [50] https://crates.io/crates/ort

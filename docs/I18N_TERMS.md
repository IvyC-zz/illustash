# 中英术语对照

现行参考，2026-10-18（#182）。界面译文的用词以 `web/static/locales/en.json` 现有译法为准，这里只列反复出现、容易译得前后不一的词；翻译方式、译文表格式和检查见 [WEB_V2.md](WEB_V2.md#界面语言2026-10-18173)。加其他语言时，先按这张表定下本语言的对应词，再译整表。改了某个词的英文时，同一个 PR 里把 `en.json` 里所有用到它的条目和这张表一起改。

## 名词

| 中文 | English | 说明 |
|---|---|---|
| 图库 | Library | 一个图库包。导出图库 Library export |
| 资料库 | Catalog | 角色、作品资料所在的页面；「还没收录」译为 not in the catalog |
| 作品 | series | 角色所属的作品。单复数同形：1 series、3 series |
| 角色 | character | |
| 原创角色 | original character | 画师自己的角色；「原创角色（未细分）」Original character (unsplit) |
| 自建角色 | characters you created | 用户自己添加、还没关联 Danbooru 的角色 |
| 造型 | variant | 角色下的变体：服装、异格、变身等，数据里是角色的 `variants`。2026-11-14 用户定（#423）界面上统一叫造型，不再分皮肤、子标签、形态；「文件形态」File form 说的不是角色，不在此列 |
| 标签 | tag | 我的标签 My tags；只当标签 Tag only |
| 画师 | artist | |
| 张（量词）、图片、图 | picture | 不用 image；「{n} 张」写成复数 `{n} picture` / `{n} pictures` |
| 原图 | original | 「读不到原图」originals could not be read |
| 副本 | copy | 重复的同一张图；副本组 copy group |
| 来源 | source | 图库读图的文件夹、iCloud 照片等 |
| 出处 | origin | 一张图在 X、Pixiv 上的原帖 |
| 识别 | recognition / identify | 名词用 recognition（识别结果 recognition results），动词与状态用 identify（已自动识别 identified automatically） |
| 角色索引 | character index | |
| 人工整理（记录） | your organizing | 用户手工做的判断，备份和撤销都围绕它 |
| 任务面板 | task panel | |
| 大图 | Large view | |
| 收藏 | saved | 收藏 {n} 张 {n} saved；收藏曲线 Collection chart |

## 页面与分组

| 中文 | English |
|---|---|
| 首页 | Home |
| 未识别 | Unidentified |
| 已跳过 | Skipped |
| 已排除 | Excluded |
| 待确认 | To confirm |
| 重复 | Duplicates |
| 相似图片 | Similar pictures |
| 相似组 N | Similar group N（未识别的分组「第 N 组」是 Group N） |
| 身份整理 | Identities |
| 随机抽查 | Spot check |
| 画师抽查 | Artist spot check |
| 找回画师 | Find artists |
| 名称翻译 | Name translation |
| 设置 › 通用 | Settings › General |
| 网络与隐私 | Network and privacy |
| 排除规则 | Exclusion rules |
| 视图选项 | View options |
| 新手引导 | Introduction |

## 动作

| 中文 | English |
|---|---|
| 同步 | Sync |
| 跳过 | Skip |
| 撤销 | Undo |
| 关联 Danbooru | Link to Danbooru |
| 合并到另一个角色 | Merge into another character |
| 按相似细分 | Split by similarity |
| 判断关系 | Compare |
| 跟随系统 | System |
| 可疑优先 | Doubtful first |

## 写法

- 标题、按钮、菜单用句首大写（View options，不写 View Options）；句子照常加句号。
- 引用界面上的名字用弯引号：the “Duplicates” page。省略号用一个字符 `…`。
- 计数放在名词前，按复数规则写（`{n} pictures`）；不方便写复数的句子改写成不依赖单复数的说法。
- `R18`、`Danbooru`、`Pixiv`、`X`、`iCloud`、`Photos` 等专名不译。

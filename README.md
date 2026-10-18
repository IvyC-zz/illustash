# Illustash

给二次元插画收藏者的本地图库：把从 X、Pixiv 存下来的成千上万张图自动按角色整理好，认错了一键改、随时撤销，还能帮你找回画师出处。全部在本机运行，图片不出你的电脑。

> **现状**：个人项目，早期阶段。macOS 是主力平台，Windows 可用。**Linux 已冻结**（2026-10-06 决定）：不提供安装包和本地计算（识别、索引、学习、溯源），不在 Linux 上做检查，Rust 代码能否在 Linux 编译也不作保证；以后有需求再基于 macOS/Windows 的原生实现补做。还没有签名的安装包和正式发布，目前需要从源码构建；界面有简体中文和英文（「设置 › 通用 › 语言」切换，默认跟随系统）。

## 能做什么

- **按角色自动整理**：用 WD Tagger 和 CCIP 两个本地模型识别角色，机器结果默认算数，看到错的才动手。单人图只保留最可能的一个角色，其余列进「也可能是」。
- **边改边学**：你确认、移出、合并的结论会被学进去，同一角色的其他图随之重新认领；人工结论永远优先，模型更新不会覆盖它。所有操作都能撤销（⌘Z）。
- **新角色与未识别**：未识别的图按相似程度分组，整组设为某个角色；首页的「认一认」卡片偶尔问你几张，帮模型补样本。
- **找回画师**：用 Danbooru 像素哈希、IQDB 签名、关注列表等线索找出画师和原帖，再在本地比对画师的公开作品带出更多图。只发哈希和文本，不上传图片；每次联网都要你授权。
- **重复与相似图**：完全相同的副本、差分、裁剪版自动归组，可以按关系折叠。
- **浏览**：拖选、框选、捏合缩放、大图查看；按角色、作品、画师、标签筛选；NSFW 可一键隐藏。
- **图库来源**：Apple 照片（只读）和普通文件夹，按出处（X、Pixiv 等）分栏。

## 隐私与底线

- **图片只留本地**：图片和从图片算出的特征都不发给任何大模型或多模态 API，也不用于训练或改进任何共享的模型（包括本软件自带的识别模型）。本软件会根据你的标注在本机学出一些只属于这个图库的打分规则（如角色原型、自建标签的候选），它们存在图库里，随标注变化重算，删除图库即消失。
- **不动你的原图**：不复制图库、不删除原图、不写 Photos 数据库。
- **联网要你同意**：所有联网功能默认关闭。角色名称和 Danbooru 搜索只发送 tag 名、作品名等文字，你在界面上允许一次后对这个图库一直有效，可随时撤销；找回画师、索引更新每次运行前单独确认。设置里的「网络与隐私」列出全部联网功能和发送的内容。保存账号或修改启动偏好都不等于授权联网。
- **第三方站点**：Pixiv、X、Danbooru、IQDB 等都是可选功能，只用你自己的账号、只读、低频；遇到人机验证或限流就停下，不绕过。

## 从源码运行

需要 Rust stable；运行时不需要 Python（构建与检查工具用到 Python 3.12 和 Node 22）。模型不在仓库里，识别功能需要另外准备，见 [安装与平台](docs/BACKEND_TASKS.md#独立安装与可选计算包)。

```sh
# 构建核心
cargo build --release --manifest-path native/terms/Cargo.toml

# 首次建库：只登记来源，不扫描图片
./native/terms/target/release/illustash-terms library-create \
    --dest ~/Pictures/Illustash/我的图库.pilib --folder /图片目录

# 打开（默认 http://127.0.0.1:8765）
./start-web.command --library ~/Pictures/Illustash/我的图库.pilib
```

桌面 App（Tauri）、安装包构建、启动参数和图库包结构见 [开发说明](docs/DEVELOPMENT.md)。

## 反馈与贡献

这是一个主要满足作者自己需求的个人项目：

- 欢迎在 Issue 里报告 bug，请用「报告问题」模板，附上系统、诊断信息和复现步骤；装不上或打不开先看 [装不上怎么办](docs/INSTALL.md)；
- 功能建议也欢迎提，但不一定会做；
- **暂不接受代码贡献（Pull Request）**。

## 许可

代码以 [GPL-3.0-or-later](LICENSE) 发布。第三方组件、模型与数据的许可见 [第三方组件与许可](docs/THIRD_PARTY.md)；模型不随代码许可，按各自许可使用。完整安装包附带的 CCIP 模型依据 BigScience Open RAIL-M 许可提供，使用和再分发受其第 5 条及附件 A 的使用限制约束。

Illustash 的名称与图标不随代码授权：基于本项目的修改版本请使用其他名称和图标。

## 致谢

角色识别使用 [SmilingWolf 的 WD Tagger](https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3) 与 [DeepGHS 的 CCIP](https://huggingface.co/deepghs/ccip) 及 [Character Index](https://huggingface.co/datasets/deepghs/character_index)；标签与角色资料来自 [Danbooru](https://danbooru.donmai.us)，角色中文名来自 [Bangumi](https://bgm.tv)。

# 离线识别结果包 v2

> 状态：现行参考；2026-11-12。离线识别 v1 由 Rust 校验、导入与恢复；机器证据经 [EVIDENCE_PACKAGES.md](EVIDENCE_PACKAGES.md) 的核心接口发布。运行边界见 [ARCHITECTURE_DIRECTION.md](ARCHITECTURE_DIRECTION.md)。

原生命令行和 HTTP 共用 `app_recognition::import_package` / `resume`；无新增界面。结果在 Windows 生成，导入在持有图库的主机执行。不从导入包加载代码、pickle 或模型。

## 文件契约

结果包是一个目录，含 `manifest.json`、`labels.jsonl`、`wd.npy`、`ccip.npy`。不接收压缩包，不解压任意路径。

```json
{
  "format": "illustash-recognition/2",
  "inputs": {"资产 ID": "实际输入文件完整字节的 SHA-256，小写 64 位十六进制"},
  "preprocessing": "illustash-wd-ccip/1",
  "library_id": "目标 library.json 中的 id",
  "assets": ["资产 ID，大小写必须与 source.sqlite 完全一致"],
  "models": {
    "wd": {"version": "swinv2_448", "dimension": 1024, "sha256": "模型文件 SHA256", "tags_sha256": "tags/tags_v3.csv SHA256"},
    "ccip": {"version": "ccip-model_feat", "dimension": 768, "sha256": "模型文件 SHA256"}
  },
  "files": {
    "labels.jsonl": {"bytes": 123, "sha256": "完整文件 SHA256"},
    "wd.npy": {"bytes": 123, "sha256": "完整文件 SHA256"},
    "ccip.npy": {"bytes": 123, "sha256": "完整文件 SHA256"}
  }
}
```

`assets` 无重复，每个 ID 必须仍在目标图库 source 的收集范围内且未移除。资产 ID 表示来源中的资产身份，不表示文件内容；相同 ID 可以先后对应不同图片。`inputs` 必须恰好覆盖这些 ID，值是识别实际读取字节的 SHA-256。`source.sqlite.assets.stable_hash` 在文件夹来源是 MD5，在 Photos 来源可能是平台标识，不作为 v2 输入版本。

`preprocessing` 固定为 `illustash-wd-ccip/1`，表示下文描述的解码、缩放、归一化与阈值语义；改变这些语义时必须升级版本，模型、标签文件另由 `models` 中的字节指纹绑定。新导入只接受 v2；v1 没有输入版本，明确报错并要求重新识别生成 v2，不允许事后替旧结果补入当前文件的哈希。已经提交的 v1 pending 账本仍可使用既有 `resume` 恢复，保留原有未绑定输入版本的语义，不能据此声称旧结果已通过版本核验。

导入在源事务内读取当前资产路径，对完整字节重新取 SHA-256，并在发布源事务之前再核对一次；文件读不到、资产不在范围内或哈希不匹配时整包拒绝，已经暂写的标签、向量和账本回滚，人工裁决不受影响。即使未运行扫描、文件名和时间戳没变，也不能导入不同内容的旧包。外部程序不参与 SQLite 锁，提交后的文件改动仍由后续扫描使结果失效；本协议不承诺锁住用户文件或跟踪文件的历史修改次数。

两个 NPY 都是 C-order little-endian float32，形状分别为 `(len(assets),1024)`、`(len(assets),768)`，行序与 assets 一致；先验证 NPY 头部声明的形状与精确字节长度，再读取数组，禁止对象类型、NaN、Inf、零向量。每个文件上限 1 GiB，最多 100,000 个资产。

labels.jsonl 每行：

```json
{"asset":"资产 ID","dim":"general","value":"1girl","confidence":0.987}
```

仅接受 general、character、rating，标签必须在本地 tags 表的对应类别；置信度须为有限的 0–1 数值，每个资产至少一条 rating，禁止重复 `(asset,dim,value)`。生产者应保留 WD 概率 >=0.001 的全部标签，与本机识别策略一致。人工判断、人工画师结论、角色身份关系不属于此包，导入器不接受也不覆盖它们。

## 原生计算与内部角色认领

`recognition_compute` 使用显式路径加载 ONNX Runtime 动态库；后端选择、CPU 线程、加速开关、缓存与回退以 [识别的执行后端](BACKEND_TASKS.md#识别的执行后端) 为准。WD 先丢弃 alpha、白底补成正方形（奇数差向下取整居中），按 Pillow BICUBIC 语义缩到 448；白边是虚拟采样，不物化 max(宽,高)² 的正方形，横向一轮只遍历落在图像列上的系数，白边部分按整数权重和一次计入，耗时随输入像素线性增长（#74），输出与物化实现逐位一致（#46），输入为未除以 255 的 BGR NHWC float32；CCIP 按 Pillow BILINEAR 语义直接缩到 384，RGB/CLIP 归一化后输入 NCHW float32。不应用 EXIF 旋转或 ICC 转换。保存模型的原始 WD 1024 / CCIP 768 向量；特征的单位化属于研究计算消费者。

生产者与导入器共用 `ModelFiles` 路径及 SHA-256 规格：默认为资源 root 的 `models/`，`ILLUSTASH_MODELS` 的绝对路径直接使用，相对路径锚定资源 root。同步把自己的资源 root 传给计算进程，标签仍由同一资源 root 的 `tags/tags_v3.csv` 提供；自定义模型根不绕过字节指纹，模型或标签变化拒绝旧结果。流式指纹读取保留合作取消检查。

内部请求升级为 `illustash-sync/2`，每项 `{id,path,input_sha256}` 由宿主对实际文件取指纹。worker 完整读入一次，对同一缓冲区校验 SHA-256 后解码和推理；指纹不同返回 `input_changed`，下次同步重试，不登记为损坏图片，也不输出该项机器结果。批次只包含成功项及其请求哈希，结果包导入仍复核当前字节，所以推理期间替换图片也不能发布旧结果。模型或输入错误不修改来源及人工记录。完成标记继续绑定 manifest 和 sidecar 的摘要，不新建调度或恢复机制。

公开 v2 不接收 `char_ext`。原生识别任务另生成 `character-claims.json`，格式为 `illustash-recognition-claims/1`，绑定完整任务身份、资产顺序、模型规格，以及共享锚点向量、ID、源角色元数据和补丁来源指纹。它只供同步任务在 `TaskContext::receive` 内部接收；公开 CLI/HTTP 结果导入没有这个入口。一个任务只构建一份锚点关系快照；全部声明的 sidecar 计算预验在接收锁外完成，并检查取消。准备证明仅保存路径与摘要，不累计保存全部批次载荷。源写事务内重新校验权威模型、锚点文件、角色元数据和 sidecar 指纹，严格复读 v2 载荷，不重建锚点关系或计算认领；声明的 sidecar 缺失、损坏、迟到或版本不匹配时拒绝写入该批 WD 结果。同步先预验全部声明的 sidecar，再开始逐包导入；逐包源事务不代表整个多包任务跨库原子提交。

原生认领保留固定契约中的直接邻居打分与严格向上别名跳转、curated p90 / `danbooru-patch` p75 阈值，检查最近 9 个锚点；同一别名先登记已见再判断阈值。距离是 float32 `(1-cos)/2`，得分为 `1-distance`，不当作 WD 概率四舍五入或裁剪。已有锚点但本次认领为空时，清除本批资产旧 `char_index/char_ext` 机器候选；没有锚点时保留旧角色候选，仍生成 WD 与原始向量。

锚点单位化复用研究计算中 NumPy float32 连续归约（8 lane、128 元素 cutoff），保留 `ccip.cross` 的零向量分母下限；元素都有限但平方和溢出的向量（锚点或待匹配特征）报「特征向量数值过大」并让任务失败，不再按全零向量计距（2026-10-12，#75）；矩阵乘法复用 Platform 的窄 CPU 数值接口。macOS 使用系统 BLAS，其他平台数值验收单列。锚点关系构建保留旧算法最多 1024 行的矩阵块；64 行分块会令末尾单行 GEMV 翻转 `.01` 别名边界。取消仍在归一化、逐别名行与 BLAS 前后检查；一次不可中断 BLAS 最多 1024 行乘全体锚点，相较 64 行上限增加 16 倍，取消等待受实际矩阵规模与平台影响，不承诺固定时长。不增加阈值 epsilon。sidecar 的 distance/threshold 按 float32 解码，confidence 为 float64，JSON 最短小数表示不改变该类型契约。

角色源标签、WD、向量和组合指纹共用已有源事务与 pending 账本。应用恢复经既有 `import_changes` 发布 `ccip_index / char_ext / ccip_score` 有效证据集，替换相同范围旧机器候选并保留人工裁决；不另建恢复调度器。组合指纹包含锚点快照与该资产认领，其他来源标签和人工记录保持现行规则。撤回 sidecar 生产路径后仍可用原生 `resume` 完成已提交 pending 记录；公开导入仍不接收内部 sidecar；v2 的无锚点去重指纹也包含输入版本。

2026-10-04 macOS arm64、CPU ONNX Runtime 1.30.0 合成对照：8 张生成图片的 10,861 维 WD 概率、1024 / 768 维原始向量与当时的 Python 参考实现（#9 已删除）逐位一致，保留阈值与四位舍入一致；同一组生成图的模型输出构造 8 条合成锚点后，真实生产得到的 14 条认领与该参考按声明类型解码逐位一致。稠密向量及相邻 float32 阈值对照确认没有用容差掩盖归约差异。首版 `fdb8e3e` 的另一次 26 项真实模型请求（25 项复用生成图片、1 项损坏输入）验证两批 24 / 1、文件指纹、原始向量、标签及无锚点 sidecar，源 sentinel 未改变。正式认领替换、空结果、人工裁决保护、预验后篡改/过期拒绝、外部合成模型根及源提交后恢复由合成数据库回归覆盖；这些结果不代表真实图片准确率、私人锚点质量、Windows、GPU 或整批全量验收。

macOS / Windows 安装 adapter 的识别请求走原生 producer，执行后端见 [识别的执行后端](BACKEND_TASKS.md#识别的执行后端)，宿主对该入口要求角色 sidecar；Linux 已冻结，不提供识别。Windows 复用固定 ILP64 OpenBLAS 资源及已登记 DLL 身份，默认仅解析 ROOT/openblas/openblas.dll，显式 ILLUSTASH_RESEARCH_OPENBLAS_DLL 优先；缺失或未知身份直接失败，不回退到 portable 实现。显式声明的 sidecar 在各平台仍严格校验。共享 CCIP 推理接口由索引计算直接复用，不需要加载 WD。

真实模型的可重复合成验收入口为 `tests.platform_recognition_models`，与普通 discover 分离，缺少显式输入直接失败：

```sh
ILLUSTASH_TERMS_BIN=/absolute/path/illustash-terms \
ILLUSTASH_RECOGNITION_ADAPTER_BIN=/absolute/path/illustash-worker \
ILLUSTASH_RECOGNITION_COMPUTE_BIN=/absolute/path/recognition-compute \
ILLUSTASH_RECOGNITION_MODELS=/absolute/path/models \
ILLUSTASH_ORT_DYLIB=/absolute/path/onnxruntime-library \
python3.12 -m unittest tests.platform_recognition_models -v -b
```

Windows 验收还须显式提供 `ILLUSTASH_RESEARCH_OPENBLAS_DLL`，与提供参考的 NumPy wheel 使用同一个已登记 DLL；两套 wheel 的矩阵 oracle 不可互换。

此入口只使用生成图片和由其模型输出构造的合成锚点；默认设 `ILLUSTASH_CPU_ONLY=1`，逐位比较概率、原始向量、阈值和认领，并验证坏图、批次和 macOS / Windows 重定位识别布局内的首次 / 增量同步、角色正式证据发布及人工记录保留。`AcceleratedRecognitionTest` 按 [识别的执行后端](BACKEND_TASKS.md#识别的执行后端) 的平台边界和数值口径验收加速路径及 CPU 回退；不能把 CPU 的逐位相等结论套用于所有加速 provider。Python 仅承担对照与夹具制作，原生子进程使用空 PATH、独立 HOME 和共享根。布局夹具仅含识别所需的显式资源，不能据此宣称签名安装包、桌面 GUI 或完整计算包通过；完整包装与资源许可证由独立交付验收覆盖。

## 使用

```sh
illustash-terms recognition-import --library /path/to/library.pilib --root /path/to/resources --model-specs
illustash-terms recognition-import --library /path/to/library.pilib --root /path/to/resources --package /path/to/result
illustash-terms recognition-import --library /path/to/library.pilib --root /path/to/resources --resume
```

`--root` 是模型与标签资源目录，不是图库路径。

模型规格命令只计算本地模型及 tag 文件的指纹，不联网。生产者必须使用同一套模型、tag 顺序和预处理。模型名称字符串相同而文件不同也会被拒绝。

HTTP：`POST /api/recognition/import`，JSON `{"path":"本机结果包目录"}`；恢复用 `{"resume":true}`。沿用已有 loopback、Origin 和 CSRF 校验，不提供远程文件下载。请求启动后台任务后立即返回，阶段、结果和取消经 `GET /api/imports`、`POST /api/imports/cancel`，见[导入任务](ARCHITECTURE_DIRECTION.md#导入任务)（#88）；CLI 仍为同步执行。

## 事务、去重与后处理

验证全部输入之后，在 source.sqlite 的一个 `BEGIN IMMEDIATE` 事务中导入机器 labels、embeddings 及 recognition_imports / recognition_assets 账本。任一 SQL 写入失败，整个包回滚；其他来源 labels、decisions 不删除。去重以每项仍有效的 `recognition_assets` 为准，不能只看历史整包收据。完全相同的包、不同包中的相同资产结果都去重；仍有效的资产结果出现不同指纹时拒绝整个包，不能静默覆盖。去重指纹覆盖输入 SHA-256、预处理版本、模型规格、标签、两个向量及适用的角色认领。同资产换内容需先由扫描使旧机器结果失效，再接受对应新输入的 v2 包；只改时间戳而字节相同不使结果失效。

历史收据保留。相同包需要补回失效资产时，使用新的 redelivery 收据和投影事件，只处理已缺失的资产；旧的 done 收据不能吞掉这次补写。恢复只处理账本仍归本收据所有的资产；v2 发布前重新检查这些资产的输入字节，旧收据不能复活已替换的图片结果。

原始识别数据提交后，更新应用索引，提交 imported 后处理通知。source 与应用数据库没有跨库伪原子事务：后处理失败保留已导入结果，账本记 `pending` 与实际错误，`--resume` 或重复导入重试。返回 `pending` 代表未全部完成；CLI 退出码为 2。应用 `meta` 的 `recognition_projection:<digest>` 事件 ID 与正式导入、imported 事件同事务提交；恢复先检查此标记，应用已提交但通知或源 done 更新失败时只重试通知/确认，不重复执行正式导入。WD 证据沿用独立核心提交和内容去重，不能宣称与应用导入同事务。

`done` 表示结果入库、索引更新及后处理通知已交付，后台算法自身状态仍由任务状态报告。基础宿主和原生 CLI 未配置研究 worker 时通知为成功 no-op，不运行学习、聚类、相似扫描；原生研究宿主通过 `app_research::dispatch(&Library,"imported")` 持久交付调度意图，随后由研究任务确认计算结果；重复结果不重复通知。失败保留事件错误和源 pending 账本。研究协议与收尾见 [架构边界](ARCHITECTURE_DIRECTION.md#原生学习聚类与相似研究协调)。未知/names 事件不由此导入器确认。

Windows 包内 `illustash-recognition` 生产者只写隔离结果目录，复用既有 WD/CCIP 预处理和阈值，沿用本协议交给 Rust 导入。2026-10-02 已用真实模型和 3 张生成图片验收识别、两类向量及正式标签导入；未验收真实图片准确率、大图库或 GPU，也未据此作跨平台性能比较。安装与依赖边界见 [独立计算包](BACKEND_TASKS.md#独立安装与可选计算包)。

公开 Rust 接口 `import_package(&Library,&Path /*结果目录*/,&Path /*资源root*/,&AtomicBool,dispatch)` 与 `resume(&Library,&AtomicBool,dispatch)` 返回原状态对象；接口内部固定 canonical 包路径（`Library::pinned`），显示别名移除或改指向不会重定位结果；调用方仍须捕获会话身份并验证迟到结果，不能仅用 library_id 定位。取消在验证、源写入和各应用提交前检查；已提交的阶段留待恢复。HTTP 显式离线导入在会话锁内验证令牌后作为后台任务执行，应用同一时间只运行一个导入；取消、切库或退出在源事务提交前停止导入，旧令牌/请求不能写入新会话（#26、#88）。只读图库拒绝导入；研究计算不得修改人工裁决。

回退：v1 源账本格式不变；原生恢复标记和事件记录保留，避免把已提交但未确认的结果当作未导入，pending 包先用 `resume` 完成确认再换版本。更旧核心版本须恢复升级前备份。

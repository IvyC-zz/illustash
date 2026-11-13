# 装不上、打不开怎么办

现行参考，2026-10-20。面向使用者，说明安装、首次启动、下载识别模型和更新时常见的拦截与报错。macOS 部分按 2026-10-15、10-16、10-18 在 macOS 26.6（arm64）虚拟机里的实测写成；Windows 部分标明了哪些已经实测。开发者从源码构建见 [开发说明](DEVELOPMENT.md)。

目前还没有正式发布的安装包（见 README「现状与运行」）。安装包没有经过 Apple 公证或 Windows 代码签名，所以系统第一次打开时会拦截，按下面的步骤放行一次即可。

## macOS

只支持 Apple 芯片（M 系列）的 Mac，需要 macOS 14 或更新版本；Intel 芯片的 Mac 打不开。

### 「无法打开"Illustash"」

第一次双击时，系统会提示无法验证 Illustash 是否包含恶意软件，只有「移到废纸篓」和「完成」两个按钮。这是因为安装包没有经过 Apple 公证，不代表文件有问题。

1. 点「完成」（不要点「移到废纸篓」）。
2. 打开「系统设置 › 隐私与安全性」，往下翻到「安全性」，会看到「已阻止"Illustash"以保护 Mac」，点旁边的「仍要打开」。
3. 再弹一次确认框时，选「仍要打开」。标准（非管理员）账户这时还要输入一位管理员的用户名和密码。

放行一次之后，以后双击就能直接打开。2026-10-18 复测时，管理员账户在第 3 步也被要求输入密码。

### 更新到新版本之后

安装包用的是临时签名，每个版本的签名都不一样，macOS 会把新版本当成另一个 App。2026-10-18 在虚拟机里用两个不同的构建实测，从网上下载新版本、覆盖安装之后：

- **要重新放行一次**：第一次打开新版本时，又会出现上面的「无法打开"Illustash"」，按同样的三步放行。
- **iCloud 照片要重新允许**：用 iCloud 照片作来源的图库，更新后第一次同步时，会再次询问是否允许 Illustash 访问照片图库，选「允许完全访问」。
- 如果选了「不允许」，任务面板会提示「没有访问照片图库的权限」（新图库的上手引导期间首页顶上也会提示），已经加入的图都在。点提示旁的「打开系统设置」（直接打开「系统设置 › 隐私与安全性 › 照片」），打开 Illustash 的开关，在系统的提示里选「退出并重新打开」，重开后同步就能读到照片（默认启动时会自动同步）。这一步不需要输入密码（2026-10-19 虚拟机实测，#202）。

图库、设置和其他图库文件夹不受影响。

### 拖进「应用程序」时要求输入管理员密码

用标准（非管理员）账户时，把 Illustash 拖进「应用程序」需要输入一位管理员的用户名和密码。这是 macOS 对标准账户的正常要求。

### 「完整安装包组件缺失：…；请重新安装 macOS 完整版」

安装包里的某个文件不见了或损坏了（例如只拷了一部分）。重新下载完整安装包，覆盖安装后**退出并重新打开** Illustash。只点「重试」不会生效，界面上也会这样提示。

### 「识别组件没能启动」

图片仍会加入图库，可以手动整理，但不会自动认出角色。先在提示里点「重新检查」；还不行的话，按下面「报告问题」附上诊断信息。

## 识别模型

安装包里没有带识别模型。第一次使用时，应用会提示「识别要用的模型还没下载」，原始模型约 618 MB，Apple 芯片的 macOS 另下载约 467 MB 的 WD 加速文件（合计约 1.1 GB）；下载前可以先建库、浏览和手动整理，模型就位后自动开始识别。模型放在用户目录里，所有图库共用，以后更新 Illustash 不用再下：

- macOS：`~/Library/Application Support/Illustash/Shared/models/`
- Windows：`%LOCALAPPDATA%\Illustash\Shared\models\`

每个文件都按应用里写好的大小和 SHA-256 核对，对不上就丢弃，所以从哪里下载都一样安全。下载中断、取消或者中途关掉应用，下好的部分都会保留，下次接着下；「设置 › 数据 › 识别模型」里可以删掉未完成的部分；不再需要时也可以在那里删除已下载的模型，之后要重新下载或导入才能识别新加入的图片，已经识别的结果都保留。卸载 Illustash 不会删除下载的模型。

### 下载很慢或下载失败

原始模型先从 Hugging Face 的镜像（默认 hf-mirror.com）下载，不行再换 ModelScope；macOS 的 WD 加速文件从项目 GitHub Release 下载，不经过镜像。下载走系统设置里的 HTTP 代理；不支持自动代理配置（PAC）。

- **换镜像**：在「设置 › 数据 › 识别模型」里把「Hugging Face 镜像」换成其他兼容 Hugging Face 的地址；填 `https://huggingface.co` 就是直连 Hugging Face。
- **自己下载再导入**：用浏览器或下载工具把下表 4 个文件下载下来，在应用里点「从文件导入…」选中它们（可以一次多选，也可以选包含它们的 ZIP）。文件名和所在文件夹都不重要，应用按大小和 SHA-256 认出每个文件，放到正确的位置；对不上的文件会被拒绝。

| 文件 | 大小（字节） | SHA-256 | Hugging Face | ModelScope |
|---|---|---|---|---|
| WD SwinV2 Tagger v3 | 467,461,052 | `ae25f5f12b2d5891728fb7768fa63d65bf13a83d6d4823e3a1bf7b42875c74ff` | [model.onnx](https://huggingface.co/deepghs/wd14_tagger_with_embeddings/resolve/4ec31f2aeabf66353f3ee3ccd20d2f07e716851b/SmilingWolf/wd-swinv2-tagger-v3/model.onnx) | [model.onnx](https://modelscope.cn/models/deepghs/wd14_tagger_with_embeddings/resolve/master/SmilingWolf/wd-swinv2-tagger-v3/model.onnx) |
| CCIP 特征模型 | 150,248,245 | `4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f` | [model_feat.onnx](https://huggingface.co/deepghs/ccip_onnx/resolve/eb2acdd29af1703388d3d0c04221add322bc9110/ccip-caformer-24-randaug-pruned/model_feat.onnx) | [model_feat.onnx](https://modelscope.cn/models/deepghs/ccip_onnx/resolve/master/ccip-caformer-24-randaug-pruned/model_feat.onnx) |
| CCIP 比对模型 | 1,649 | `7e4646fdfbe369ad485b227ec47ac4092c98b8deef3af44e19fdbd8ba7bc25c1` | [model_metrics.onnx](https://huggingface.co/deepghs/ccip_onnx/resolve/eb2acdd29af1703388d3d0c04221add322bc9110/ccip-caformer-24-randaug-pruned/model_metrics.onnx) | [model_metrics.onnx](https://modelscope.cn/models/deepghs/ccip_onnx/resolve/master/ccip-caformer-24-randaug-pruned/model_metrics.onnx) |
| CCIP 参数 | 147 | `b5535577288e22940b853192cd1661e45c7615b7d89b49a8745c2c81685552d4` | [metrics.json](https://huggingface.co/deepghs/ccip_onnx/resolve/eb2acdd29af1703388d3d0c04221add322bc9110/ccip-caformer-24-randaug-pruned/metrics.json) | [metrics.json](https://modelscope.cn/models/deepghs/ccip_onnx/resolve/master/ccip-caformer-24-randaug-pruned/metrics.json) |

Hugging Face 的地址固定在上游的某个提交上，上游以后改动也不影响；镜像站把 `https://huggingface.co` 换成镜像地址即可。下载后可以自己核对：macOS 用 `shasum -a 256 文件`，Windows 用 `certutil -hashfile 文件 SHA256`。模型的许可见 [第三方组件与许可](THIRD_PARTY.md#模型与数据)。

### 识别加速（macOS）

在 Apple 芯片的 Mac 上，识别默认通过 Core ML 加速标签与角色特征计算。WD 加速文件由项目模型 Release 提供，首次下载模型时一并下载；已有安装可在「设置 › 数据 › 识别模型」补下载。手动下载 [WD 加速文件 v1](https://github.com/shio-vinyl/illustash/releases/download/wd-coreml-v1/wd-swinv2-v3-v1.onnx) 后也可从设置导入：文件为 467,387,780 字节，SHA-256 为 `57344d061ecaa0b8addb9cc33ae1a133a5bb18525590fdc61edb2aacdad2e444`。它需要与原始 WD 模型共同保留，不能用它替换原始文件。

首次识别会准备编译缓存，WD 和 CCIP 分别保存在 `~/Library/Application Support/Illustash/Shared/coreml-cache/` 下。模型、运行库或 macOS 版本变化时，相应模型的缓存会重新生成。加速文件缺失、损坏，或编译、运行失败时，识别自动回到原始模型 CPU，设置页显示原因；已有图库和模型身份不受影响。想只用 CPU，关闭「使用 GPU 加速识别」，下次识别时生效。

**隐私**（#381）：macOS 随包的 ONNX Runtime 是微软官方发布的版本，默认开启遥测，会把使用事件发给微软（`mobile.events.data.microsoft.com`），并在 `~/Library/Application Support/Microsoft/DeveloperTools/.onnxruntime/` 写一个设备标识。Illustash 启动每个识别、索引进程时都设 `ORT_DISABLE_TELEMETRY=1`，并在初始化时调用关闭遥测的接口，所以它不联网，也不写那个目录。如果你在 Illustash 之外用过别的 ONNX Runtime 程序，那个目录可能是它们留下的。

## Windows

- 安装包装在当前用户下，不需要管理员权限。
- 安装包没有代码签名，下载后第一次运行时，Windows SmartScreen 可能提示「Windows 已保护你的电脑」。点「更多信息」，再点「仍要运行」。**这一条还没有在本项目的安装包上实测过**；首个内测版只在开发用的 Windows 机器上验收过，没有在干净的系统上装过。遇到这里没写的拦截或报错，请按下面「报告问题」反馈。
- 装好后从开始菜单或桌面上的「Illustash」快捷方式启动。
- **识别默认用显卡加速**（DirectML，2026-10-21 起，#271）：需要支持 DirectX 12 的显卡，有独立显卡的笔记本会用独立显卡。不想用时，在「设置 › 数据 › 识别模型」里关掉「使用 GPU 加速识别」，之后全部用 CPU，下次识别时生效。没有合适的显卡、驱动出错，或者显卡在识别中途出错时，会自动改用 CPU 接着识别，设置里会显示上次没能用上加速的原因。用不用显卡，分级、标签和角色判定都一样，只有置信度的最后一位小数偶尔不同。**只有核显的电脑还没有测过**，提升可能很小；如果觉得反而变慢，关掉这个开关即可。
- **隐私**：随包的 ONNX Runtime 由本项目从源码构建，不带微软的遥测配置，它的诊断事件不进 Windows 的遥测通道；Illustash 还在初始化时调用关闭遥测的接口（#381）。GPU 加速用的是微软的 DirectML 组件，随安装包提供。它登记在 Windows 的遥测通道里（2026-11-13 核对随包文件，#381），按它的许可，可能依照 Windows 的「诊断数据」设置向微软发送使用信息；实际上传了什么，本项目没有抓包核实。不想用的话，在「设置 › 数据 › 识别模型」里关掉「使用 GPU 加速识别」，识别就只用 CPU，不再加载 DirectML。

## 报告问题

用仓库的 Issue「报告问题」模板。请附上「设置 › 关于 › 复制诊断信息」的内容，里面只有版本、构建号、系统和组件状态，不含路径。不要贴真实图片、图片文件名、本机路径或账号信息。

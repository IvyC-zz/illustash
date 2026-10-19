# 装不上、打不开怎么办

现行参考，2026-10-19。面向使用者，说明安装、首次启动、下载识别模型和更新时常见的拦截与报错。macOS 部分按 2026-10-15、10-16、10-18 在 macOS 26.6（arm64）虚拟机里的实测写成；Windows 部分标明了哪些已经实测。开发者从源码构建见 [开发说明](DEVELOPMENT.md)。

目前还没有正式发布的安装包（见 README「现状」）。安装包没有经过 Apple 公证或 Windows 代码签名，所以系统第一次打开时会拦截，按下面的步骤放行一次即可。

## macOS

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
- 如果选了「不允许」，同步会报「数据库错误：unable to open database file：…Photos.sqlite」，已经加入的图都在。到「系统设置 › 隐私与安全性 › 照片」里打开 Illustash 的开关，按提示退出并重新打开 Illustash，再同步一次即可。这一步不需要输入密码。

图库、设置和其他图库文件夹不受影响。

### 拖进「应用程序」时要求输入管理员密码

用标准（非管理员）账户时，把 Illustash 拖进「应用程序」需要输入一位管理员的用户名和密码。这是 macOS 对标准账户的正常要求。

### 「完整安装包组件缺失：…；请重新安装 macOS 完整版」

安装包里的某个文件不见了或损坏了（例如只拷了一部分）。重新下载完整安装包，覆盖安装后**退出并重新打开** Illustash。只点「重试」不会生效，界面上也会这样提示。

### 「识别组件没能启动」

图片仍会加入图库，可以手动整理，但不会自动认出角色。先在提示里点「重新检查」；还不行的话，按下面「报告问题」附上诊断信息。

## 识别模型

安装包里没有带识别模型。第一次使用时，应用会提示「识别要用的模型还没下载」，点「下载模型」后下载约 618 MB；下载前可以先建库、浏览和手动整理，模型就位后自动开始识别。模型放在用户目录里，所有图库共用，以后更新 Illustash 不用再下：

- macOS：`~/Library/Application Support/Illustash/Shared/models/`
- Windows：`%LOCALAPPDATA%\Illustash\Shared\models\`

每个文件都按应用里写好的大小和 SHA-256 核对，对不上就丢弃，所以从哪里下载都一样安全。下载中断、取消或者中途关掉应用，下好的部分都会保留，下次接着下；「设置 › 数据 › 识别模型」里可以删掉未完成的部分。

### 下载很慢或下载失败

应用先从 Hugging Face 的镜像（默认 hf-mirror.com）下载，不行再换 ModelScope。下载走系统设置里的 HTTP 代理；不支持自动代理配置（PAC）。

- **换镜像**：在「设置 › 数据 › 识别模型」里把「Hugging Face 镜像」换成其他兼容 Hugging Face 的地址；填 `https://huggingface.co` 就是直连 Hugging Face。
- **自己下载再导入**：用浏览器或下载工具把下表 4 个文件下载下来，在应用里点「从文件导入…」选中它们（可以一次多选，也可以选包含它们的 ZIP）。文件名和所在文件夹都不重要，应用按大小和 SHA-256 认出每个文件，放到正确的位置；对不上的文件会被拒绝。

| 文件 | 大小（字节） | SHA-256 | Hugging Face | ModelScope |
|---|---|---|---|---|
| WD SwinV2 Tagger v3 | 467,461,052 | `ae25f5f12b2d5891728fb7768fa63d65bf13a83d6d4823e3a1bf7b42875c74ff` | [model.onnx](https://huggingface.co/deepghs/wd14_tagger_with_embeddings/resolve/4ec31f2aeabf66353f3ee3ccd20d2f07e716851b/SmilingWolf/wd-swinv2-tagger-v3/model.onnx) | [model.onnx](https://modelscope.cn/models/deepghs/wd14_tagger_with_embeddings/resolve/master/SmilingWolf/wd-swinv2-tagger-v3/model.onnx) |
| CCIP 特征模型 | 150,248,245 | `4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f` | [model_feat.onnx](https://huggingface.co/deepghs/ccip_onnx/resolve/eb2acdd29af1703388d3d0c04221add322bc9110/ccip-caformer-24-randaug-pruned/model_feat.onnx) | [model_feat.onnx](https://modelscope.cn/models/deepghs/ccip_onnx/resolve/master/ccip-caformer-24-randaug-pruned/model_feat.onnx) |
| CCIP 比对模型 | 1,649 | `7e4646fdfbe369ad485b227ec47ac4092c98b8deef3af44e19fdbd8ba7bc25c1` | [model_metrics.onnx](https://huggingface.co/deepghs/ccip_onnx/resolve/eb2acdd29af1703388d3d0c04221add322bc9110/ccip-caformer-24-randaug-pruned/model_metrics.onnx) | [model_metrics.onnx](https://modelscope.cn/models/deepghs/ccip_onnx/resolve/master/ccip-caformer-24-randaug-pruned/model_metrics.onnx) |
| CCIP 参数 | 147 | `b5535577288e22940b853192cd1661e45c7615b7d89b49a8745c2c81685552d4` | [metrics.json](https://huggingface.co/deepghs/ccip_onnx/resolve/eb2acdd29af1703388d3d0c04221add322bc9110/ccip-caformer-24-randaug-pruned/metrics.json) | [metrics.json](https://modelscope.cn/models/deepghs/ccip_onnx/resolve/master/ccip-caformer-24-randaug-pruned/metrics.json) |

Hugging Face 的地址固定在上游的某个提交上，上游以后改动也不影响；镜像站把 `https://huggingface.co` 换成镜像地址即可。下载后可以自己核对：macOS 用 `shasum -a 256 文件`，Windows 用 `certutil -hashfile 文件 SHA256`。模型的许可见 [第三方组件与许可](THIRD_PARTY.md#模型与数据)。

## Windows

- 安装包装在当前用户下，不需要管理员权限。
- 安装包没有代码签名，下载后第一次运行时，Windows SmartScreen 可能提示「Windows 已保护你的电脑」。点「更多信息」，再点「仍要运行」。**这一条还没有在本项目的安装包上实测过。**
- 装好后从开始菜单或桌面上的「Illustash」快捷方式启动。

## 报告问题

用仓库的 Issue「报告问题」模板。请附上「设置 › 关于 › 复制诊断信息」的内容，里面只有版本、构建号、系统和组件状态，不含路径。不要贴真实图片、图片文件名、本机路径或账号信息。

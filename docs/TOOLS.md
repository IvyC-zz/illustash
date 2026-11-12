# tools/ 目录用途

状态：现行参考，2026-10-08。本文列出 `tools/` 下保留的脚本及用途；保留与删除的依据见 #186，安装包里实际带哪些脚本以 `packaging/build.py` 的白名单为准（见 [后端与安装说明](BACKEND_TASKS.md#独立安装与可选计算包)）。

旧 Python 索引工具及其自测已于 2026-10-08 按用户决定删除（#53）。正式索引计算由原生 `illustash-worker` 完成；平台模型专项用原生 CLI 准备合成认领输入，算法比较沿用提交的固定期望。`tools/` 只保留构建、检查与验收脚本，Python 源码不进安装包。

## 构建、检查与验收

| 脚本 | 用途 |
|---|---|
| `check_all.sh`、`local_acceptance.py` | 本地全量检查与固定路径验收，见 [LOCAL_CHECKS.md](LOCAL_CHECKS.md) |
| `check_architecture.py` | crate 依赖方向检查 |
| `check_frontend.py` | 前端结构检查：`web/static` 的依赖方向、state 声明、全局监听、对话框、超长行；基线 `frontend_baseline.json` 只能缩小（#320） |
| `migration_inventory.py` | 生产迁移边界清点，规格在 `tests/migration_inventory.json` |
| `gen_casefold.py`、`gen_http_unicode.py` | 生成 `crates/core` 的 casefold 与 Unicode 表 |
| `test_native_contracts.sh`、`bench_native_contracts.sh` | 空 PATH 重放固定契约；契约基准，见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md) |
| `bench_recognition.py` | 整应用识别吞吐基准（合成图片，扫描到识别、导入完成），见 [TERMS_PERFORMANCE.md 第 6 节](TERMS_PERFORMANCE.md#6-整应用识别吞吐基准) |
| `verify_read_http.py` | 原生测试用的 HTTP 逐字节对照辅助 |
| `build_provenance_probe.py`、`windows_acceptance.py`、`windows_research_acceptance.py` | 溯源候选构建、Windows 基础版与研究包验收 |
| `build_db.py` | 重建旧格式分析源库；`test_native_library_upgrade` 用它造旧库 |
| `dev_preview.py` | 浏览器走查用的隔离宿主（临时目录、合成图库），`.claude/launch.json` 的 `web-isolated` 调用它，见 [开发说明](DEVELOPMENT.md#快速开始) |

研究脚本已全部移出本仓库（2026-10-06、2026-10-07）。

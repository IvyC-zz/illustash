# tools/ 目录用途

状态：现行参考，2026-10-07。本文列出 `tools/` 下保留的脚本及用途；保留与删除的依据见 #186，安装包里实际带哪些脚本以 `packaging/build.py` 的白名单为准（见 [后端与安装说明](BACKEND_TASKS.md#独立安装与可选计算包)）。

## 旧 Python 计算脚本（只作测试参考）

2026-10-07 起不进任何安装包（Linux 已冻结，见 [README](../README.md)），只被原生计算的对照测试引用；改为固定期望后随 `tests/reference/` 一起删除（#9）。

| 脚本 | 用途 |
|---|---|
| `index_worker.py` | 角色索引更新 worker，只写 Rust 准备的隔离目录 |
| `ccip.py` | CCIP 角色相似度（两阶段实现，见 [ERRATA.md](../ERRATA.md) 第 1、7 条） |
| `char_patch.py`、`build_characters.py`、`char_scan.py` | 补外部角色索引、角色实体表、锚点全库扫描；`index_worker` 调用 |
| `calibrate_classification.py`、`validation_worksheet.py` | `web/learn.py` 引用的阈值校准与盲审工作表 |

## 构建、检查与验收

| 脚本 | 用途 |
|---|---|
| `check_all.sh`、`local_acceptance.py` | 本地全量检查与固定路径验收，见 [LOCAL_CHECKS.md](LOCAL_CHECKS.md) |
| `check_architecture.py` | crate 依赖方向检查 |
| `migration_inventory.py` | 生产迁移边界清点，规格在 `tests/migration_inventory.json` |
| `gen_casefold.py`、`gen_http_unicode.py` | 生成 `crates/core` 的 casefold 与 Unicode 表 |
| `test_native_contracts.sh`、`bench_native_contracts.sh` | 空 PATH 重放固定契约；契约基准，见 [TERMS_PERFORMANCE.md](TERMS_PERFORMANCE.md) |
| `verify_read_http.py` | 原生测试用的 HTTP 逐字节对照辅助 |
| `build_provenance_probe.py`、`windows_acceptance.py`、`windows_research_acceptance.py` | 溯源候选构建、Windows 基础版与研究包验收 |
| `build_db.py` | 重建旧格式分析源库；`test_native_library_upgrade` 用它造旧库 |

研究脚本已全部移出本仓库（2026-10-06、2026-10-07）。

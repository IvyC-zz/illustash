#!/usr/bin/env bash
# 本地全量入口，对齐 .github/workflows/ci.yml；只用仓库合成数据。
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
head=$(git rev-parse HEAD)
git diff --quiet && git diff --cached --quiet || { echo "全量验收要求已提交的干净工作区。" >&2; exit 1; }
workflow="$root/.github/workflows/ci.yml"
config() { awk -v key="$1" '$1 == key ":" {gsub(/\047/, "", $2); print $2; exit}' "$workflow"; }
python_version=$(config PYTHON_VERSION)
python_bootstrap=${CHECK_ALL_PYTHON:-python$python_version}
"$python_bootstrap" - "$python_version" <<'PYTHON'
import sys
expected = tuple(map(int, sys.argv[1].split('.')))
if sys.version_info[:2] != expected:
    raise SystemExit(f'需要 CI 的 Python {sys.argv[1]}，当前为 {sys.version.split()[0]}')
PYTHON
[ "$(node -p 'process.versions.node.split(".")[0]')" = 22 ] || { echo '需要 CI 的 Node 22。' >&2; exit 1; }
# 禁止外部 target 覆盖破坏测试按仓库路径定位二进制的约定。
unset CARGO_TARGET_DIR
cache="$root/.venv/check-all"
mkdir -p "$cache"
"$python_bootstrap" -m venv "$cache/python"
python="$cache/python/bin/python"
"$python" -m pip install -r tests/requirements.txt
export PATH="$cache/python/bin:$PATH"
# Linux 已冻结（README）；Windows 单列验收。
[ "$(uname -s)" = Darwin ] || { echo '当前脚本仅支持 macOS；Linux 已冻结，Windows 需另行验收。' >&2; exit 2; }
printf '本地全量开始：head=%s，平台=%s %s\n' "$head" "$(uname -s)" "$(uname -m)"
"$python" --version
node --version
cargo --version
run() { printf '\n== %s ==\n' "$*"; "$@"; }
# 纯工具合成测试及测试文件语法尽早验证；改动相关测试可额外前置。
run "$python" -m compileall -q tests
run "$python" tools/check_architecture.py
run "$python" -m unittest discover -s tests -p test_local_acceptance.py -v -b
# 旧变量不能在 release 前调用任意测试，避免读取上一候选的二进制。
if [ -n "${CHECK_ALL_PREFLIGHT_TESTS:-}" ]; then
  echo 'CHECK_ALL_PREFLIGHT_TESTS 已移除；依赖候选构建的定向项请用 CHECK_ALL_TARGETED_TESTS（构建后执行）。' >&2
  exit 2
fi
run cargo fmt --manifest-path crates/core/Cargo.toml --check
run cargo clippy --locked --manifest-path crates/core/Cargo.toml --all-targets -- -D warnings
run cargo test --locked --manifest-path crates/core/Cargo.toml
run cargo build --locked --manifest-path crates/core/Cargo.toml
run cargo fmt --manifest-path crates/platform/Cargo.toml --check
run cargo clippy --locked --manifest-path crates/platform/Cargo.toml --all-targets -- -D warnings
run cargo test --locked --manifest-path crates/platform/Cargo.toml
run cargo build --locked --manifest-path crates/platform/Cargo.toml
run cargo fmt --manifest-path native/terms/Cargo.toml --check
run cargo clippy --locked --manifest-path native/terms/Cargo.toml --all-targets --all-features -- -D warnings
# 前置较快检查和桌面构建，避免 Python 全量之后才发现构建失败。
run node --test tests/*.test.cjs tests/test_pixiv_poll.js
run cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets -- -D warnings
run cargo test --locked --manifest-path desktop/Cargo.toml
run cargo test --locked --manifest-path native/terms/Cargo.toml
run cargo fmt --manifest-path crates/iqdb/Cargo.toml --check
run cargo clippy --locked --manifest-path crates/iqdb/Cargo.toml --all-targets -- -D warnings
run cargo test --locked --manifest-path crates/iqdb/Cargo.toml
run cargo build --release --locked --manifest-path native/terms/Cargo.toml
run cargo build --release --locked --manifest-path native/terms/Cargo.toml --features test-read-host --example read-http --example recognition-compute --example index-compute --example research-compute --example relation-probe --example provenance-compute
run cargo build --release --locked --manifest-path crates/iqdb/Cargo.toml
export ILLUSTASH_TERMS_BIN="$root/native/terms/target/release/illustash-terms"
export ILLUSTASH_NATIVE_ROOT="$root"
export ILLUSTASH_IQDB_BINARY="$root/crates/iqdb/target/release/illustash-iqdb"
export ILLUSTASH_READ_HTTP_BIN="$root/native/terms/target/release/examples/read-http"
export ILLUSTASH_RECOGNITION_COMPUTE_BIN="$root/native/terms/target/release/examples/recognition-compute"
export ILLUSTASH_INDEX_COMPUTE_BIN="$root/native/terms/target/release/examples/index-compute"
export ILLUSTASH_RELATION_PROBE_BIN="$root/native/terms/target/release/examples/relation-probe"
export ILLUSTASH_PROVENANCE_COMPUTE_BIN="$root/native/terms/target/release/examples/provenance-compute"
export ILLUSTASH_RESEARCH_COMPUTE_BIN="$root/native/terms/target/release/examples/research-compute"
export ILLUSTASH_RESEARCH_ADAPTER_BIN="$root/native/terms/target/release/illustash-worker"
export ILLUSTASH_INDEX_ADAPTER_BIN="$root/native/terms/target/release/illustash-worker"
# 依赖核心/测试读宿主/IQDB/旧包的定向测试只在本次候选构建后运行。
if [ -n "${CHECK_ALL_TARGETED_TESTS:-}" ]; then
  printf '构建后定向测试：候选 head=%s；二进制来自本轮固定路径构建。\n' "$head"
  read -r -a targeted_tests <<< "$CHECK_ALL_TARGETED_TESTS"
  run "$python" -m unittest "${targeted_tests[@]}" -v -b
fi
log="$cache/unittest.log"
# 失败时仍输出完整跳过清单，随后保留 unittest 的失败状态。
status=0
run "$python" -m tests.run_fixed_suite -v -b 2>&1 | tee "$log" || status=$?
grep -E ' \.\.\. skipped ' "$log" > "$cache/skips.txt" || true
printf '\nPython 跳过：%s 项\n' "$(wc -l < "$cache/skips.txt" | tr -d ' ')"
cat "$cache/skips.txt"
tail -n 3 "$log"
allowed='full-features\.npy not present|POSIX|posix|Unix|macOS/Linux only|Windows pinned OpenBLAS backend only|explicit Windows native development calculator only|explicit Windows development App publication only|Linux is frozen: no local computation'
if grep -Ev "$allowed" "$cache/skips.txt"; then
  echo '以上跳过不属于平台限制或真实图库数据缺失，视为未验收。' >&2
  status=1
fi
[ "$status" -eq 0 ] || exit "$status"
# 默认包含手动 CI 的 contracts job；用 Python 解析 Cargo JSON 后，空 PATH 重放。
run cargo test --release --locked --manifest-path native/terms/Cargo.toml --test contracts --no-run --message-format=json > "$cache/contracts-artifacts.jsonl"
binary=$("$python" - "$cache/contracts-artifacts.jsonl" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    try:
        item = json.loads(line)
    except json.JSONDecodeError:
        continue
    if item.get('reason') == 'compiler-artifact' and item['target']['name'] == 'contracts' and item.get('executable'):
        print(item['executable'])
PY
)
[ -n "$binary" ] && [ -x "$binary" ]
run tools/test_native_contracts.sh "$binary"
[ "$(git rev-parse HEAD)" = "$head" ] && git diff --quiet && git diff --cached --quiet || {
  echo '验收期间 head 或已跟踪文件改变，不能计为最终 head 全通过。' >&2
  exit 1
}
printf '\n本地全量通过：head=%s，平台=%s %s；未覆盖其他操作系统。\n' "$head" "$(uname -s)" "$(uname -m)"

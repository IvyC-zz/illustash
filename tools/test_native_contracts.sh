#!/bin/sh
# 在没有 Python、Cargo 和系统 PATH 的情况下运行已构建好的契约可执行文件。
set -eu
runner=${1:?usage: test_native_contracts.sh /absolute/release-or-debug/contracts-executable}
case "$runner" in /*) ;; *) echo 'absolute executable required' >&2; exit 2;; esac
[ -x "$runner" ]
owned=$(mktemp -d "${TMPDIR:-/tmp}/illustash-no-python.XXXXXX")
trap 'rm -rf "$owned"' EXIT HUP INT TERM
mkdir "$owned/bin" "$owned/home"
/usr/bin/env -i PATH="$owned/bin" HOME="$owned/home" TMPDIR="$owned" "$runner" --test-threads=1

#!/bin/sh
# Run the already-built contract executable with no Python/Cargo/system PATH.
set -eu
runner=${1:?usage: test_native_contracts.sh /absolute/release-or-debug/contracts-executable}
case "$runner" in /*) ;; *) echo 'absolute executable required' >&2; exit 2;; esac
[ -x "$runner" ]
owned=$(mktemp -d "${TMPDIR:-/tmp}/illustash-no-python.XXXXXX")
trap 'rm -rf "$owned"' EXIT HUP INT TERM
mkdir "$owned/bin" "$owned/home"
/usr/bin/env -i PATH="$owned/bin" HOME="$owned/home" TMPDIR="$owned" "$runner" --test-threads=1

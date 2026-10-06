#!/bin/zsh
set -eu
cd "${0:A:h}"
if [[ -n "${ILLUSTASH_TERMS_BIN:-}" ]]; then
  CORE="$ILLUSTASH_TERMS_BIN"
else
  CORE=""
  for candidate in native/terms/target/release/illustash-terms native/terms/target/debug/illustash-terms; do
    if [[ -x "$candidate" && ( -z "$CORE" || "$candidate" -nt "$CORE" ) ]]; then
      CORE="$PWD/$candidate"
    fi
  done
fi
if [[ -z "$CORE" || ! -x "$CORE" ]]; then
  echo '缺少词条核心程序，请先构建 native/terms 或设置 ILLUSTASH_TERMS_BIN。' >&2
  exit 1
fi
exec "$CORE" http --root "$PWD" "$@"

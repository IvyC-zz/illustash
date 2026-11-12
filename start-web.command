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
# 宿主打印带一次性码的地址，加 --open 时用默认浏览器打开它；页面只能从这个地址使用。
# 在它后面加 --no-open 则不打开浏览器。
exec "$CORE" http --root "$PWD" --open "$@"

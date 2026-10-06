#!/bin/bash
# 下载 deepghs/character_index 的全部角色锚点特征 + 元数据。
#
#   bash tools/fetch_char_index.sh [并发数]
#
# 网络策略（主人要求尽量不吃 Shadowrocket 流量）
#   1. 先试 en0 直连 huggingface.co（绑定源接口 + 公共 DNS 自己解析 + --noproxy）
#   2. 直连不通再走代理，并在日志里明确写出来
#
#   2026-09-22 实测：直连 huggingface.co **完全不通**（75 s 超时，96/96 全失败）；
#   hf-mirror.com 能连上但对 LFS 文件 302 回源 huggingface.co，等于没用。
#   所以现在跑起来一定是「代理模式」，约 1.0 GB 流量。
#
# 可续跑：已存在且 >200 B 的文件直接跳过。
# 产物 review/char-index/<short_tag>.npy（N×768 CCIP 特征）+ .json（core_tags/copyright/post_count）
set +u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/review/char-index"
LIST="$ROOT/review/char-index.dirs"
P="${1:-64}"
BASE="https://huggingface.co/datasets/deepghs/character_index/resolve/main"
mkdir -p "$OUT"

# ---- 1. 试直连 ----
EN0="$(/usr/sbin/ipconfig getifaddr en0 2>/dev/null)"
RESOLVE=(); NOK=0; NEED=0
if [ -n "$EN0" ]; then
  for d in huggingface.co us.aws.cdn.hf.co cas-bridge.xethub.hf.co transfer.xethub.hf.co; do
    NEED=$((NEED+1)); ip=""
    for ns in 223.5.5.5 114.114.114.114 119.29.29.29; do
      ip="$(/usr/bin/dig +short +time=3 +tries=1 -b "$EN0" @$ns "$d" 2>/dev/null | grep -E '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$' | tail -1)"
      [ -n "$ip" ] && break
    done
    [ -n "$ip" ] && { RESOLVE+=(--resolve "$d:443:$ip"); NOK=$((NOK+1)); }
  done
fi
MODE="proxy"
if [ "$NOK" -eq "$NEED" ] && [ "$NEED" -gt 0 ]; then
  RESOLVE+=(--interface "$EN0" --noproxy "*")
  # 真正探一下：DNS 解析成功不代表能连（GFW 是 TCP 层阻断）
  probe="$(env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
    curl -sSL -m 25 "${RESOLVE[@]}" -o /dev/null -w '%{http_code}' \
    "$BASE/c9/mika_blue_archive/data.json" 2>/dev/null)"
  if [ "$probe" = "200" ]; then MODE="direct"; fi
fi

if [ "$MODE" = "direct" ]; then
  echo "✅ 直连模式：en0=$EN0，不走 Shadowrocket"
  CURL=(env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy curl "${RESOLVE[@]}")
else
  echo "⚠️  直连不通（探测返回 '${probe:-无}'），回退 Shadowrocket 代理 —— 本次约 1.0 GB 流量"
  RESOLVE=()
  CURL=(curl)
fi

# ---- 2. 角色目录清单 ----
if [ ! -s "$LIST" ]; then
  echo "拉取目录清单 ..."
  "${CURL[@]}" -sS -m 120 -H 'User-Agent: Mozilla/5.0' \
    "https://huggingface.co/api/datasets/deepghs/character_index" \
  | "$ROOT/.venv/bin/python" -c "
import json,sys
d=json.load(sys.stdin)
print('\n'.join(sorted({s['rfilename'].rsplit('/',1)[0] for s in d['siblings'] if '/' in s['rfilename']})))" > "$LIST"
fi
TOTAL=$(grep -c . "$LIST")
HAVE=$(ls "$OUT" 2>/dev/null | wc -l | tr -d ' ')
echo "角色 $TOTAL 个 / 已有 $HAVE 个文件，并发 $P，产物 $OUT"

i=0; t0=$(date +%s)
while read -r p; do
  [ -z "$p" ] && continue
  n="$(basename "$p")"
  for f in feat.npy data.json; do
    case "$f" in feat.npy) dst="$OUT/$n.npy";; *) dst="$OUT/$n.json";; esac
    if [ -s "$dst" ]; then
      sz=$(stat -f%z "$dst" 2>/dev/null || echo 0)
      [ "$sz" -gt 200 ] && continue
    fi
    "${CURL[@]}" -sSL -m 120 -H 'User-Agent: Mozilla/5.0' -o "$dst" "$BASE/$p/$f" 2>/dev/null &
    i=$((i+1))
    [ $((i % P)) -eq 0 ] && wait
  done
  if [ $((i % 1000)) -lt 2 ] && [ "$i" -gt 0 ]; then
    echo "  [$(date +%H:%M:%S)] 发起 $i 请求 / 落盘 $(ls "$OUT" | wc -l | tr -d ' ') 文件 / $(( $(date +%s) - t0 ))s / $(du -sh "$OUT" | cut -f1)"
  fi
done < "$LIST"
wait
echo "完成：$(ls "$OUT"/*.npy 2>/dev/null | wc -l | tr -d ' ') npy / $(ls "$OUT"/*.json 2>/dev/null | wc -l | tr -d ' ') json，用时 $(( $(date +%s) - t0 ))s，$(du -sh "$OUT" | cut -f1)"

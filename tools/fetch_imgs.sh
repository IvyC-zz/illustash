#!/bin/bash
# 从内网 Windows 拉图到本地缓存 —— 绕开 macOS 的 Photos 库权限限制。
#
#   echo -e "000123.jpeg\n004567.jpeg" | bash tools/fetch_imgs.sh
#   bash tools/fetch_imgs.sh files.txt
#
# 背景：Claude 这个会话读 ~/Pictures/Photos Library.photoslibrary/resources/derivatives/
# 会被 macOS TCC 拒（Operation not permitted），所以拼图做不出来。
# 但 Windows 上 E:\photo-intel\full\imgs\ 有一份与 manifest 逐字节一致的副本，
# 局域网直连，几十张图一两秒就回来了。
#
# 缓存在 review/imgcache/，已存在的跳过。
set +u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="$ROOT/review/imgcache"
HOST="${PI_WIN_HOST:?设置 PI_WIN_HOST 为存放副本的 Windows 主机名}"
mkdir -p "$CACHE"
LIST="$(mktemp)"
if [ -n "$1" ] && [ -f "$1" ]; then cat "$1" > "$LIST"; else cat > "$LIST"; fi
NEED="$(mktemp)"
while read -r f; do
  [ -z "$f" ] && continue
  [ -s "$CACHE/$f" ] || echo "$f"
done < "$LIST" > "$NEED"
n=$(grep -c . "$NEED")
if [ "$n" -eq 0 ]; then echo "全部已缓存（$(grep -c . "$LIST") 张）"; rm -f "$LIST" "$NEED"; exit 0; fi
echo "要拉 $n 张 ..."
# 分批 scp：一批给太多路径会静默拿不到文件（实测 23 个就会），20 个一批稳。
BATCH=20
ERR="$(mktemp)"
split -l $BATCH "$NEED" "$NEED.part." 2>/dev/null || cp "$NEED" "$NEED.part.aa"
for part in "$NEED".part.*; do
  [ -s "$part" ] || continue
  args=""
  while read -r f; do [ -n "$f" ] && args="$args $HOST:E:/photo-intel/full/imgs/$f"; done < "$part"
  # shellcheck disable=SC2086
  scp -o ConnectTimeout=15 $args "$CACHE/" 2>>"$ERR" || true
done
rm -f "$NEED".part.*
[ -s "$ERR" ] && { echo "  scp 报错（前 3 行）:"; head -3 "$ERR" | sed 's/^/    /'; }
rm -f "$ERR"
got=$(while read -r f; do [ -s "$CACHE/$f" ] && echo x; done < "$NEED" | wc -l | tr -d ' ')
echo "拿到 $got/$n，缓存在 $CACHE"
rm -f "$LIST" "$NEED"

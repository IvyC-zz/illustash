#!/bin/sh
# 同一份固定的合成数据集，明确指定的 release 宿主，正好 3 轮独立试验。
set -eu
runner=${1:?usage: bench_native_contracts.sh /absolute/release/contracts /absolute/main-host /absolute/candidate-host}
baseline=${2:?explicit main release host required}
candidate=${3:?explicit candidate release host required}
for path in "$runner" "$baseline" "$candidate"; do
    case "$path" in /*) ;; *) echo 'absolute executables required' >&2; exit 2;; esac
    [ -f "$path" ] && [ -x "$path" ] || { echo "missing executable: $path" >&2; exit 2; }
done
owned=$(mktemp -d "${TMPDIR:-/tmp}/illustash-benchmark.XXXXXX")
trap 'rm -rf "$owned"' EXIT HUP INT TERM
mkdir "$owned/bin" "$owned/home"
run_contract() {
    /usr/bin/env -i PATH="$owned/bin" HOME="$owned/home" TMPDIR="$owned" \
        ILLUSTASH_BENCH_ROOT="$owned/trial" ILLUSTASH_BENCH_BINARY="$2" \
        "$runner" --ignored --exact "$1" --nocapture > "$owned/trial.log" 2>&1 || { cat "$owned/trial.log" >&2; exit 1; }
}
# 只扩容、迁移一次。合成记录含绝对路径，所以每轮把准备好的副本放回同一个试验目录。
run_contract prepare_benchmark_fixture ''
# 一个物理上不同的图库包使用同一个图库 ID；每次切换都必须打开那个实例。
cp -R "$owned/trial/fixture.pilib" "$owned/trial/replacement.pilib"
mv "$owned/trial" "$owned/prepared"
: > "$owned/samples.tsv"
for trial in 1 2 3; do
    for label in baseline candidate; do
        if [ "$label" = baseline ]; then host=$baseline; else host=$candidate; fi
        cp -R "$owned/prepared" "$owned/trial"
        run_contract benchmark_release_host "$host"
        awk -v label="$label" -v trial="$trial" '$1 == "BENCH" {print label, $2, trial, $3}' "$owned/trial.log" >> "$owned/samples.tsv"
        rm -rf "$owned/trial"
    done
done
# 读取和人工操作取每轮的平均值；打开取 7 次启动的中位数，切换取 20 次切换的中位数。
# 每个延迟都包含宿主实际的生命周期和回环 IO；报告三轮数值的中位数。
awk '
function median(a,b,c,t) {if(a>b){t=a;a=b;b=t} if(b>c){t=b;b=c;c=t} if(a>b){t=a;a=b;b=t} return b}
{n[$1,$2]++; v[$1,$2,$3]=$4; keys[$2]=1}
END {
    print "dataset: 4096 synthetic assets, 2 characters, same-ID distinct packages; 7 opens/20 switches per trial; 3 trials; MiB = 1048576 bytes"
    print "metric baseline_median candidate_median delta acceptance"
    for (k in keys) {
        if(n["baseline",k]!=3 || n["candidate",k]!=3) {exit 3}
        b=median(v["baseline",k,1],v["baseline",k,2],v["baseline",k,3]); c=median(v["candidate",k,1],v["candidate",k,2],v["candidate",k,3]);
        if(k=="rss_mib") {d=c-b; ok=d<10; printf "%s %.6f %.6f %+.6fMiB %s\n",k,b,c,d,ok?"accepted":"exceeds-threshold"}
        else {d=(c/b-1)*100; ok=d<20; printf "%s_ms %.6f %.6f %+.2f%% %s\n",k,b,c,d,ok?"accepted":"exceeds-threshold"}
        if(!ok) failed=1
    }
    exit failed
}' "$owned/samples.tsv"

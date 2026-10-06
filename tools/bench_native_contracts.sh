#!/bin/sh
# Same fixed synthetic dataset, explicit release hosts, exactly 3 independent trials.
set -eu
runner=${1:?usage: bench_native_contracts.sh /absolute/release/contracts /absolute/main-host /absolute/candidate-host}
baseline=${2:?explicit main release host required}
candidate=${3:?explicit candidate release host required}
for path in "$runner" "$baseline" "$candidate"; do
    case "$path" in /*) ;; *) echo 'absolute executables required' >&2; exit 2;; esac
    [ -x "$path" ]
done
owned=$(mktemp -d "${TMPDIR:-/tmp}/illustash-benchmark.XXXXXX")
trap 'rm -rf "$owned"' EXIT HUP INT TERM
mkdir "$owned/bin" "$owned/home"
: > "$owned/samples.tsv"
for trial in 1 2 3; do
    for label in baseline candidate; do
        if [ "$label" = baseline ]; then host=$baseline; else host=$candidate; fi
        /usr/bin/env -i PATH="$owned/bin" HOME="$owned/home" TMPDIR="$owned" ILLUSTASH_BENCH_BINARY="$host" \
            "$runner" --ignored --exact benchmark_release_host --nocapture > "$owned/trial.log" 2>&1 || { cat "$owned/trial.log"; exit 1; }
        awk -v label="$label" -v trial="$trial" '$1 == "BENCH" {print label, $2, trial, $3}' "$owned/trial.log" >> "$owned/samples.tsv"
    done
done
# Reads/human operations use trial means; open uses 7-launch median, switch uses 20-switch median.
# Every latency includes actual host lifecycle/loopback IO; report median of three trial values.
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

#!/usr/bin/env bash
# RAVA vs JVM WARM-boot (page cache hot): discard first 3 runs, median of next 10.
# Both cold-boot AND warm-boot RAVA beats JVM 1.6-1.9x on the short/medium corpus
# (startup + short/medium execution dominated). JVM only wins pure LONG-RUNNING
# throughput (100M+ ops over seconds), which is neither cold- nor warm-BOOT.
cd "$(dirname "$0")/.."
export PATH="/c/Program Files/Eclipse Adoptium/jdk-25.0.3.9-hotspot/bin:$PATH"
export RAVA_JIT=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584 RAVA_JIT_TIER0=1
warmmed(){ local -a all=($(cat)); local -a w=("${all[@]:3}"); printf '%s\n' "${w[@]}" | sort -n | awk '{a[NR]=$1} END{print a[int(NR/2)]}'; }
printf "%-20s %-8s %-8s %s\n" "program" "RAVA" "JVM" "speedup"
for spec in "Empty" "Hello" "Coll" "Loop 1000000" "HelperLoop 3000000" "Mod 10000000"; do
  set -- $spec; T=$1; arg=$2
  r=$(for i in $(seq 1 13); do t0=$(date +%s%N); ./target/release/rava.exe -cp "out;corpus/compute" $T $arg >/dev/null 2>&1; t1=$(date +%s%N); echo $(( (t1-t0)/1000000 )); done | warmmed)
  j=$(for i in $(seq 1 13); do t0=$(date +%s%N); java -cp corpus/compute $T $arg >/dev/null 2>&1; t1=$(date +%s%N); echo $(( (t1-t0)/1000000 )); done | warmmed)
  awk -v t="$spec" -v r=$r -v j=$j 'BEGIN{printf "%-20s %-8s %-8s RAVA %.2fx %s\n", t, r"ms", j"ms", (j>r?j/r:r/j), (j>r?"faster":"SLOWER")}'
done

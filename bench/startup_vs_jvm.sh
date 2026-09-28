#!/usr/bin/env bash
# RAVA vs JVM on the short-program corpus. RAVA wins by DEFAULT (lean process +
# loop-eager compile substitutes for OSR) on startup-bound AND compute-in-main/helper
# short programs. Warm long-running is JVM's (C2); RAVA's niche is short cold runs.
cd "$(dirname "$0")/.."
export PATH="/c/Program Files/Eclipse Adoptium/jdk-25.0.3.9-hotspot/bin:$PATH"
export RAVA_JIT=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584 RAVA_JIT_TIER0=1
med(){ local -a v=($(sort -n)); echo "${v[$((${#v[@]}/2))]}"; }
printf "%-20s %-8s %-8s %s\n" "program" "RAVA" "JVM" "speedup"
for spec in "Empty" "Hello" "Str" "Coll" "Loop 1000000" "Loop 5000000" "HelperLoop 3000000"; do
  set -- $spec; T=$1; arg=$2
  r=$(for i in $(seq 1 9); do t0=$(date +%s%N); ./target/release/rava.exe -cp "out;corpus/compute" $T $arg >/dev/null 2>&1; t1=$(date +%s%N); echo $(( (t1-t0)/1000000 )); done | med)
  j=$(for i in $(seq 1 9); do t0=$(date +%s%N); java -cp corpus/compute $T $arg >/dev/null 2>&1; t1=$(date +%s%N); echo $(( (t1-t0)/1000000 )); done | med)
  awk -v t="$spec" -v r=$r -v j=$j 'BEGIN{printf "%-20s %-8s %-8s RAVA %.2fx %s\n", t, r"ms", j"ms", j/r, (j>r?"faster":"SLOWER")}'
done

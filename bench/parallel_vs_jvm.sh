#!/usr/bin/env bash
# RAVA AUTO-PARALLELISM (data-parallel-reduce, DEFAULT-ON, opt-out RAVA_NO_JIT_PARALLEL).
# RAVA auto-detects counted associative-reduction loops (acc OP= f(i), trip>=10000) and
# forks them across OS cores GIL-free — SOUND by construction (thread-local partials,
# no shared state, no barrier). JVM does NOT auto-multithread loops. Bit-exact.
cd "$(dirname "$0")/.."
export PATH="/c/Program Files/Eclipse Adoptium/jdk-25.0.3.9-hotspot/bin:$PATH"
export RAVA_JIT=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584 RAVA_JIT_TIER0=1
med(){ local -a v=($(sort -n)); echo "${v[$((${#v[@]}/2))]}"; }
printf "%-18s %-9s %-11s %-8s %s\n" "workload" "serial" "PARALLEL" "JVM" "par/vsJVM"
for spec in "SumS 500000000" "SumHeavy 500000000"; do
  set -- $spec; T=$1; N=$2
  s=$(for i in 1 2 3 4 5; do t0=$(date +%s%N); RAVA_NO_JIT_PARALLEL=1 ./target/release/rava.exe -cp "out;corpus/compute" $T $N >/dev/null 2>&1; t1=$(date +%s%N); echo $(( (t1-t0)/1000000 )); done | med)
  p=$(for i in 1 2 3 4 5; do t0=$(date +%s%N); ./target/release/rava.exe -cp "out;corpus/compute" $T $N >/dev/null 2>&1; t1=$(date +%s%N); echo $(( (t1-t0)/1000000 )); done | med)
  j=$(for i in 1 2 3 4 5; do t0=$(date +%s%N); java -cp corpus/compute $T $N >/dev/null 2>&1; t1=$(date +%s%N); echo $(( (t1-t0)/1000000 )); done | med)
  cj=$(java -cp corpus/compute $T $N); cp=$(./target/release/rava.exe -cp "out;corpus/compute" $T $N 2>/dev/null)
  awk -v t="$spec" -v s=$s -v p=$p -v j=$j -v ok="$([ "$cj" = "$cp" ] && echo bit-exact || echo DIVERGE)" 'BEGIN{printf "%-18s %-9s %-11s %-8s %.1fx/%.1fx [%s]\n", t, s"ms", p"ms", j"ms", s/p, j/p, ok}'
done

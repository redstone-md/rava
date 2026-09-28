#!/usr/bin/env bash
# Parallel-reduction bench: JVM (C2) vs rava serial JIT vs rava parallel JIT.
# Proves rava beats the JVM when a hot loop is an associative pure-arithmetic fold
# the parallel analyzer can fan across cores. SUM must match all three ways.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/out"; RAVA="${RAVA:-$HERE/../target/release/rava.exe}"
ITERS="${ITERS:-200000000}"; TH="${RAVA_JIT_THRESHOLD:-5}"
mkdir -p "$OUT"; javac -d "$OUT" "$HERE/ParReduce.java" || exit 1
field() { grep -oE "$1=-?[0-9]+" | head -1 | cut -d= -f2; }
run() { timeout 120 env "$@" "$RAVA" -cp "$OUT" ParReduce "$ITERS" 2>/dev/null; }

jo=$(timeout 120 java -cp "$OUT" ParReduce "$ITERS")
so=$(run RAVA_JIT=1 RAVA_JIT_THRESHOLD="$TH")
po=$(run RAVA_JIT=1 RAVA_JIT_PARALLEL=1 RAVA_JIT_THRESHOLD="$TH")
jn=$(echo "$jo" | field HOTNS); sn=$(echo "$so" | field HOTNS); pn=$(echo "$po" | field HOTNS)
js=$(echo "$jo" | field SUM);  ss=$(echo "$so" | field SUM);  ps=$(echo "$po" | field SUM)
ok="ok"; { [ "$js" = "$ss" ] && [ "$js" = "$ps" ]; } || ok="MISMATCH j=$js s=$ss p=$ps"
printf "JVM=%dms  rava-ser=%dms  rava-par=%dms\n" $((jn/1000000)) $((sn/1000000)) $((pn/1000000))
awk "BEGIN{printf \"par-scaling=%.1fx  par-vs-JVM=%.2fx (%s)  CORRECT=%s\n\", $sn/$pn, $pn/$jn, ($pn<$jn?\"rava FASTER\":\"JVM faster\"), \"$ok\"}"

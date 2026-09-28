#!/usr/bin/env bash
# Array-reduction bench: JVM (C2) vs rava parallel JIT. Proves rava beats the JVM on the
# canonical `sum += a[i]` fold over a primitive array. SUM must match.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/out"; RAVA="${RAVA:-$HERE/../target/release/rava.exe}"
ITERS="${ITERS:-120000000}"; TH="${RAVA_JIT_THRESHOLD:-3}"
mkdir -p "$OUT"; javac -d "$OUT" "$HERE/ArrReduce.java" || exit 1
field() { grep -oE "$1=-?[0-9]+" | head -1 | cut -d= -f2; }

jo=$(timeout 120 java -cp "$OUT" ArrReduce "$ITERS")
po=$(timeout 120 env RAVA_JIT=1 RAVA_JIT_PARALLEL=1 RAVA_JIT_THRESHOLD="$TH" "$RAVA" -cp "$OUT" ArrReduce "$ITERS" 2>/dev/null)
jn=$(echo "$jo" | field HOTNS); pn=$(echo "$po" | field HOTNS)
js=$(echo "$jo" | field SUM);  ps=$(echo "$po" | field SUM)
ok="ok"; [ "$js" = "$ps" ] || ok="MISMATCH j=$js p=$ps"
printf "JVM=%dms  rava-par=%dms\n" $((jn/1000000)) $((pn/1000000))
awk "BEGIN{printf \"par-vs-JVM=%.2fx (%s)  CORRECT=%s\n\", $pn/$jn, ($pn<$jn?\"rava FASTER\":\"JVM faster\"), \"$ok\"}"

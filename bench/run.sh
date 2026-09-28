#!/usr/bin/env bash
# Generic (no-Minecraft) micro-benchmark harness: proves rava's per-op speed +
# universality vs the JVM. Run per phase-gate; both correctness (SUM match) and
# the hot-loop ns ratio must hold. server.jar is a separate end-to-end proxy.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/out"
RAVA="${RAVA:-$HERE/../target/release/rava.exe}"
BENCHES="ArithLoop StringOps Collections Allocs Dispatch DispatchMono Statics Fields FieldsRef"
TIMEOUT="${TIMEOUT:-180}"

rm -rf "$OUT"; mkdir -p "$OUT"
# Compile only the listed benches — extra sources (e.g. FastUtil.java, which needs
# the fastutil jar on the classpath) are run standalone, not through this harness.
SRCS=""; for b in $BENCHES; do SRCS="$SRCS $HERE/$b.java"; done
echo "compiling..."; javac -d "$OUT" $SRCS || { echo "javac failed"; exit 1; }

field() { grep -oE "$1=[-0-9]+" | head -1 | cut -d= -f2; }
printf "%-12s %12s %12s %9s  %s\n" BENCH JAVA_ns RAVA_ns "R/J" CORRECT
for b in $BENCHES; do
  jo=$(timeout "$TIMEOUT" java -cp "$OUT" "$b" 2>/dev/null)
  # Threshold 10 so the benches' 15 warmup invocations trigger compilation
  # (the production default is much higher to spend compile slots on truly hot
  # methods; these microbenches ARE the hot method).
  ro=$(RAVA_JIT="${RAVA_JIT:-0}" RAVA_JIT_THRESHOLD="${RAVA_JIT_THRESHOLD:-10}" timeout "$TIMEOUT" "$RAVA" -cp "$OUT" "$b" 2>/dev/null)
  jns=$(echo "$jo" | field HOTNS); jsum=$(echo "$jo" | field SUM)
  rns=$(echo "$ro" | field HOTNS); rsum=$(echo "$ro" | field SUM)
  [ -z "$jns" ] && jns=0; [ -z "$rns" ] && rns=0
  if [ -z "$rsum" ]; then ok="RAVA-FAIL"; elif [ "$jsum" = "$rsum" ]; then ok="ok"; else ok="MISMATCH j=$jsum r=$rsum"; fi
  if [ "$jns" -gt 0 ] && [ "$rns" -gt 0 ]; then ratio=$(awk "BEGIN{printf \"%.1fx\", $rns/$jns}"); else ratio="-"; fi
  printf "%-12s %12s %12s %9s  %s\n" "$b" "$jns" "$rns" "$ratio" "$ok"
done

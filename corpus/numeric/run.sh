#!/bin/sh
# Numeric differential harness: every probe here prints DETERMINISTIC text only,
# so `java -Xint` and rava must agree line for line. Compiles into a caller-named
# directory so parallel runs do not share `out`.
#
#   sh corpus/numeric/run.sh            # all probes, rava's compiler OFF
#   RAVA_JIT=1 sh corpus/numeric/run.sh IntSem   # one probe, compiler ON
set -u
cd "$(dirname "$0")/../.." || exit 1
OUT=out_numeric
JIT=${RAVA_JIT:-0}
PROBES=${*:-"IntSem FpSem FpSem2 NumMisc NumMisc2 CharBox MathSweep FmtSweep FtoaSweep"}

javac -d "$OUT" corpus/numeric/*.java || exit 1
fail=0
for P in $PROBES; do
  java -Xint -cp "$OUT" "$P" 2>&1 | tr -d '\r' > "$OUT/$P.jvm"
  RAVA_JIT="$JIT" RAVA_JIT_TIER0=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584 \
    ./target/release/rava.exe -cp "$OUT" "$P" 2>&1 | tr -d '\r' \
    | grep -v '^rava: OS process' > "$OUT/$P.rava"
  if diff -q "$OUT/$P.jvm" "$OUT/$P.rava" > /dev/null; then
    echo "PASS $P (JIT=$JIT)"
  else
    echo "FAIL $P (JIT=$JIT)"; diff "$OUT/$P.jvm" "$OUT/$P.rava" | head -20; fail=1
  fi
done
echo "fail=$fail"
exit $fail

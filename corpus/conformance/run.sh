#!/usr/bin/env bash
# Permanent RAVA conformance runner. For each test: RAVA lambda-PIC OFF and ON
# must match the JVM-recorded expected output (CR-normalized). Exit 1 on mismatch.
set -u
cd "$(dirname "$0")/../.."
RAVA=./target/release/rava.exe
export RAVA_JIT=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584 RAVA_JIT_TIER0=1
CP="out;corpus/conformance"
fail=0
norm(){ tr -d '\r'; }
for exp in corpus/conformance/expected/*.txt; do
  T=$(basename "$exp" .txt)
  want=$(norm < "$exp")
  for pic in 0 1; do
    got=$(RAVA_LAMBDA_PIC=$pic timeout 120 "$RAVA" -cp "$CP" "$T" 2>/dev/null | norm)
    if [ "$got" = "$want" ]; then echo "PASS $T (lambda-PIC=$pic)"
    else echo "FAIL $T (lambda-PIC=$pic)"; diff <(echo "$want") <(echo "$got") | head -8; fail=1; fi
  done
done
[ $fail -eq 0 ] && echo "ALL CONFORMANCE PASS (JVM == RAVA OFF == RAVA ON)"
exit $fail

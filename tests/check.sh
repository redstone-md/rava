#!/bin/bash
# The checks a change must pass (CI runs them on every push): the semantic probes against
# HotSpot's output, with exactly the four known red ones (helpful NullPointerException texts),
# the JNI test and the patches test. Usage: tests/check.sh [all]; `all` also runs the probes
# with every method compiled by the optimizing tier and with few allocatable registers.
# R overrides the rava binary; java and javac on PATH supply the reference.
T=$(dirname "$0")
want="ExcSem JitNpeSiteSem JitThrowNullSem NpeSem"
modes=def
[ "$1" = all ] && modes="def all few"
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
status=0
for mode in $modes; do
  case $mode in
    def) out=$("$T/semantics/run.sh" "$W/sem" 2>&1) ;;
    all) out=$(RAVA_OPT=all "$T/semantics/run.sh" "$W/sem" 2>&1) ;;
    few) out=$(RAVA_OPT_FEW_REGS=2 RAVA_OPT=all "$T/semantics/run.sh" "$W/sem" 2>&1) ;;
  esac
  got=$(echo "$out" | grep "^FAIL" | awk '{print $2}' | sort | tr '\n' ' ' | sed 's/ $//')
  echo "semantics ($mode): $(echo "$out" | tail -1) [$got]"
  [ "$got" = "$want" ] || { echo "semantics ($mode): expected exactly [$want] to fail"; status=1; }
done
"$T/jni/run.sh" "$W/jni" || status=1
"$T/patches/run.sh" "$W/patches" || status=1
exit $status

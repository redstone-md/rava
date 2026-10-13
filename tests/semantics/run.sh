#!/bin/bash
# Compiles every *Sem.java here and compares rava's output with HotSpot's (timing lines
# are ignored). Usage: tests/semantics/run.sh [work-dir]; R overrides the rava binary,
# SEM_DIFF=1 prints how a failing probe's output differs; SEM_CP adds library jars (guava, fastutil) to the classpath of the probes that import them.
# HotSpot's output is the reference, computed once per compiled probe and kept in the work
# dir (runs in other modes reuse it); fast-throw is off so its exception messages do not
# depend on when C2 compiled the throwing site.
# Four probes stay red on purpose: HotSpot's helpful NullPointerException texts (ExcSem,
# JitNpeSiteSem, JitThrowNullSem, NpeSem) are guest-visible output rava does not produce yet.
# A probe's `// rava-env: VAR=value ...` line sets environment variables for rava's run (to
# keep methods out of the optimizing tier, say).
R=${R:-$(dirname "$0")/../../target/release/rava}
W=${1:-$(mktemp -d)}
# macOS has no timeout(1) unless coreutils is installed.
command -v timeout >/dev/null || timeout() { shift; "$@"; }
filter() { grep -v "Picked up" | grep -viE "ms|time|elapsed|ns/"; }
pass=0; fail=0
for f in "$(dirname "$0")"/*Sem.java; do
  n=$(basename "$f" .java); d=$W/$n
  # Probes in a package (DynClassSem) have their own harness.
  grep -q "^package " "$f" && continue
  if [ -z "$SEM_CP" ] && grep -qE "^import (com\.google|it\.unimi)" "$f"; then echo "skip $n (needs SEM_CP)"; continue; fi
  mkdir -p "$d"
  cp="$d${SEM_CP:+:$SEM_CP}"
  if [ ! -f "$d/$n.class" ] || [ "$f" -nt "$d/$n.class" ]; then
    rm -f "$d"/*.class "$d/expected.txt"
    javac -nowarn -cp "$cp" -d "$d" "$f" 2>/dev/null || { echo "skip $n (javac; SEM_CP?)"; continue; }
  fi
  [ -f "$d/expected.txt" ] || timeout 60 java -XX:-OmitStackTraceInFastThrow -cp "$cp" "$n" 2>&1 | filter > "$d/expected.txt"
  penv=$(sed -n 's|^// rava-env: ||p' "$f")
  got=$(env $penv timeout 60 "$R" -cp "$cp" "$n" 2>&1 | filter)
  if [ "$(cat "$d/expected.txt")" == "$got" ]; then pass=$((pass+1)); else
    fail=$((fail+1)); echo "FAIL $n"
    [ -n "$SEM_DIFF" ] && diff <(echo "$got") "$d/expected.txt" | head -20 | sed 's/^/    /'
  fi
done
echo "pass=$pass fail=$fail"

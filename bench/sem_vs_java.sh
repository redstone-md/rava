#!/bin/bash
# Compiles every bench/*Sem.java and compares rava-core's output with HotSpot's (timing lines
# are ignored). Usage: bench/sem_vs_java.sh [work-dir]; R overrides the rava-core binary,
# SEM_CP adds library jars (guava, fastutil) to the classpath of the probes that import them.
# HotSpot's output is the reference, computed once per compiled probe and kept in the work
# dir (runs in other modes reuse it); fast-throw is off so its exception messages do not
# depend on when C2 compiled the throwing site.
# Four probes stay red on purpose: HotSpot's helpful NullPointerException texts (ExcSem,
# JitNpeSiteSem, JitThrowNullSem, NpeSem) are guest-visible output rava-core does not produce yet.
R=${R:-$(dirname "$0")/../target/release/rava-core}
W=${1:-$(mktemp -d)}
filter() { grep -v "Picked up" | grep -viE "ms|time|elapsed|ns/"; }
pass=0; fail=0
for f in "$(dirname "$0")"/*Sem.java; do
  n=$(basename "$f" .java); d=$W/$n
  # Probes in a package (DynClassSem) have their own harness.
  grep -q "^package " "$f" && continue
  mkdir -p "$d"
  cp="$d${SEM_CP:+:$SEM_CP}"
  if [ ! -f "$d/$n.class" ] || [ "$f" -nt "$d/$n.class" ]; then
    rm -f "$d"/*.class "$d/expected.txt"
    javac -nowarn -cp "$cp" -d "$d" "$f" 2>/dev/null || { echo "skip $n (javac; SEM_CP?)"; continue; }
  fi
  [ -f "$d/expected.txt" ] || timeout 60 java -XX:-OmitStackTraceInFastThrow -cp "$cp" "$n" 2>&1 | filter > "$d/expected.txt"
  got=$(timeout 60 "$R" -cp "$cp" "$n" 2>&1 | filter)
  if [ "$(cat "$d/expected.txt")" == "$got" ]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL $n"; fi
done
echo "pass=$pass fail=$fail"

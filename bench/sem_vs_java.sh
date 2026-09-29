#!/bin/bash
# Compiles every bench/*Sem.java and compares rava-core's output with HotSpot's (timing lines
# are ignored). Usage: bench/sem_vs_java.sh [work-dir]; R overrides the rava-core binary.
R=${R:-$(dirname "$0")/../target/release/rava-core}
W=${1:-$(mktemp -d)}
pass=0; fail=0
for f in "$(dirname "$0")"/*Sem.java; do
  n=$(basename "$f" .java); d=$W/$n; mkdir -p "$d"
  [ -f "$d/$n.class" ] || javac -nowarn -d "$d" "$f" 2>/dev/null || { echo "skip $n (javac)"; continue; }
  exp=$(timeout 60 java -cp "$d" "$n" 2>&1 | grep -v "Picked up" | grep -viE "ms|time|elapsed|ns/")
  got=$(timeout 60 "$R" -cp "$d" "$n" 2>&1 | grep -viE "ms|time|elapsed|ns/")
  if [ "$exp" == "$got" ]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL $n"; fi
done
echo "pass=$pass fail=$fail"

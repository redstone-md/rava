#!/bin/bash
# Boots Minestom (an open-source Minecraft server) on HotSpot and on rava and prints each
# phase: registry bootstrap, parallel chunk generation, network start.
# Usage: bench/minestom/run.sh [runs]   (needs Maven Central for the first dependency fetch)
set -e
cd "$(dirname "$0")"
R=${R:-../../target/release/rava}
[ -d lib ] || mvn -q -B dependency:copy-dependencies -DoutputDirectory=lib -DincludeScope=runtime
CP="$(ls lib/*.jar | tr '\n' ':')"
mkdir -p out && javac -d out -cp "$CP" src/MsBench.java
for i in $(seq "${1:-3}"); do
  echo "java:      $(java -cp "out:$CP" MsBench 2>/dev/null | grep MSBENCH)"
  echo "rava: $($R -cp "out:$CP" MsBench 2>/dev/null | grep MSBENCH)"
done

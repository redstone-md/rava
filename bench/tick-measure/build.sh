#!/usr/bin/env bash
set -euo pipefail
# Usage: JAVA_HOME=<jdk21> build.sh PAPER_BASE OUTPUT_DIRECTORY
BASE=$(cd "$1" && pwd)
mkdir -p "$2"
OUT=$(cd "$2" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
JAVAC=${JAVA_HOME:+$JAVA_HOME/bin/}javac
JAR=${JAVA_HOME:+$JAVA_HOME/bin/}jar
CP="$BASE/versions/1.21.1/paper-1.21.1.jar"
LIBS=$(mktemp "$OUT/libpaths.XXXXXX")
trap 'rm -f "$LIBS"' EXIT
find "$BASE/libraries" -name '*.jar' -print0 | sort -z > "$LIBS"
while IFS= read -r -d '' lib; do CP="$CP:$lib"; done < "$LIBS"
mkdir -p "$OUT/classes"
"$JAVAC" --release 21 -proc:none -cp "$CP" -d "$OUT/classes" "$HERE/TickMeasure.java"
cp "$HERE/plugin.yml" "$OUT/classes/plugin.yml"
"$JAR" --create --file "$OUT/TickMeasure.jar" -C "$OUT/classes" .
echo "$OUT/TickMeasure.jar"

#!/bin/bash
# build.sh SERVER_DIR [OUT_JAR]: compiles the patch against the jars of a NeoForge 1.21.1
# server (SERVER_DIR/libraries) into OUT_JAR (default parallel-levels.jar here).
set -e
here=$(realpath "$(dirname "$0")"); server=$(realpath "$1"); out=$(realpath -m "${2:-$here/parallel-levels.jar}")
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
cp=$(find "$server/libraries" -name "*.jar" | tr '\n' ':')
# The stubs show the members the patch opens (public there) to javac only; they are not packaged.
javac --release 21 -nowarn -cp "$cp" -d "$work/stubs" $(find "$here/stubs" -name "*.java")
javac --release 21 -nowarn -cp "$work/stubs:$cp" -d "$work/out" $(find "$here/src" -name "*.java")
cp "$here/rava-patch.txt" "$work/out/"
jar cf "$out" -C "$work/out" .
echo "$out"

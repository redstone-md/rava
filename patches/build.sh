#!/bin/bash
# build.sh PATCH_DIR SERVER_DIR [OUT_JAR]: compiles a patch (PATCH_DIR/src, its
# rava-patch.txt, and PATCH_DIR/stubs if any) against the jars of a NeoForge 1.21.1 server
# (SERVER_DIR/libraries and mods) into OUT_JAR (default PATCH_DIR/<name>.jar).
set -e
dir=$(realpath "$1"); server=$(realpath "$2"); name=$(basename "$dir")
out=$(realpath -m "${3:-$dir/$name.jar}")
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
cp=$(find "$server/libraries" -name "*.jar" | tr '\n' ':')
# Stubs show javac the members a patch opens (public there); they are not packaged.
if [ -d "$dir/stubs" ]; then
  javac --release 21 -nowarn -cp "$cp" -d "$work/stubs" $(find "$dir/stubs" -name "*.java")
fi
javac --release 21 -nowarn -cp "$work/stubs:$cp" -d "$work/out" $(find "$dir/src" -name "*.java")
cp "$dir/rava-patch.txt" "$work/out/"
jar cf "$out" -C "$work/out" .
echo "$out"

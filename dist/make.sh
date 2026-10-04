#!/bin/bash
# Builds a self-contained rava distribution: the binary and the Java class library it runs,
# laid out as a Java runtime is, so no JDK needs to be installed where it runs.
#
#   dist/make.sh [JDK] [OUT]
#
# JDK (default: $JAVA_HOME, else the JDK of `java` on PATH) supplies the class library: its
# `lib/modules` image (the platform classes' bytecode), the data files library code reads from
# `java.home` (time zones, security, configuration) and `legal/` (OpenJDK is GPLv2 with the
# Classpath Exception; its notices travel with it). Nothing native comes from it: rava
# implements the JDK's native methods itself. OUT (default dist/out/rava) gets:
#
#   bin/rava                        the VM
#   lib/modules, lib/*.dat, lib/security/, lib/jfr/   the class library and its data
#   conf/, legal/, release
#
# and dist/out/rava.tar.gz.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
JDK=${1:-${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")}}
OUT=${2:-$REPO/dist/out/rava}
[ -f "$JDK/lib/modules" ] || { echo "no class library at $JDK/lib/modules" >&2; exit 1; }
(cd "$REPO" && cargo build --release -q)
rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/lib"
cp "$REPO/target/release/rava" "$OUT/bin/rava"
cp -L "$JDK/lib/modules" "$OUT/lib/"
for f in "$JDK"/lib/*.dat "$JDK"/lib/*.properties; do [ -e "$f" ] && cp -L "$f" "$OUT/lib/"; done
for d in security jfr; do [ -d "$JDK/lib/$d" ] && cp -rL "$JDK/lib/$d" "$OUT/lib/"; done
cp -rL "$JDK/conf" "$OUT/conf"
cp -rL "$JDK/legal" "$OUT/legal" 2>/dev/null || true
[ -f "$JDK/release" ] && cp -L "$JDK/release" "$OUT/release"
tar -C "$(dirname "$OUT")" -czf "$(dirname "$OUT")/$(basename "$OUT").tar.gz" "$(basename "$OUT")"
du -sh "$OUT" "$(dirname "$OUT")/$(basename "$OUT").tar.gz"

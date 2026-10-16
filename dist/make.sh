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
#   bin/rava (rava.exe on Windows)   the VM
#   lib/modules, lib/*.dat, lib/security/, lib/jfr/   the class library and its data
#   conf/, legal/, release
#
# and dist/out/rava.tar.gz (rava.zip on Windows).
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
JDK=${1:-${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")}}
OUT=${2:-$REPO/dist/out/rava}
[ -f "$JDK/lib/modules" ] || { echo "no class library at $JDK/lib/modules" >&2; exit 1; }
(cd "$REPO" && cargo build --release -q)
rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/lib"
case $(uname -s) in MINGW*|MSYS*|CYGWIN*) EXE=.exe ;; *) EXE= ;; esac
cp "$REPO/target/release/rava$EXE" "$OUT/bin/rava$EXE"
cp -L "$JDK/lib/modules" "$OUT/lib/"
# tzmappings: Windows time zone names to Java IDs (only in a Windows JDK).
for f in "$JDK"/lib/*.dat "$JDK"/lib/*.properties "$JDK"/lib/tzmappings; do [ -e "$f" ] && cp -L "$f" "$OUT/lib/"; done
for d in security jfr; do [ -d "$JDK/lib/$d" ] && cp -rL "$JDK/lib/$d" "$OUT/lib/"; done
cp -rL "$JDK/conf" "$OUT/conf"
cp -rL "$JDK/legal" "$OUT/legal" 2>/dev/null || true
[ -f "$JDK/release" ] && cp -L "$JDK/release" "$OUT/release"
# rava's own license and notice, which every redistribution keeps.
cp "$REPO/LICENSE" "$REPO/NOTICE" "$OUT/"
# Windows: Mesa's OpenGL driver for -Drava.opengl=vulkan (Windows has no Mesa of its own), from
# a pinned mesa-dist-win release checked against its digest; its licenses go to legal/mesa.
if [ -n "$EXE" ]; then
  MESA_URL=https://github.com/pal1000/mesa-dist-win/releases/download/26.2.4/mesa3d-26.2.4-release-msvc.7z
  MESA_SHA256=351fc8c8b695878ffb3eaa044b3ead08672a48b1a045e3c3e3975811df0f6695
  SEVENZIP=$(command -v 7z || echo "/c/Program Files/7-Zip/7z.exe")
  MESA_TMP=$(mktemp -d)
  curl -fsSL -o "$MESA_TMP/mesa.7z" "$MESA_URL"
  echo "$MESA_SHA256  $MESA_TMP/mesa.7z" | sha256sum -c --quiet
  "$SEVENZIP" e -y -o"$MESA_TMP/x64" "$MESA_TMP/mesa.7z" x64/opengl32.dll x64/libgallium_wgl.dll > /dev/null
  mkdir -p "$OUT/lib/mesa" "$OUT/legal/mesa"
  cp "$MESA_TMP/x64/opengl32.dll" "$MESA_TMP/x64/libgallium_wgl.dll" "$OUT/lib/mesa/"
  cp "$REPO"/dist/mesa/* "$OUT/legal/mesa/"
  rm -rf "$MESA_TMP"
fi
if [ -n "$EXE" ]; then
  ARCHIVE="$(dirname "$OUT")/$(basename "$OUT").zip"
  (cd "$(dirname "$OUT")" && powershell -NoProfile -Command "Compress-Archive -Force -Path '$(basename "$OUT")' -DestinationPath '$(basename "$ARCHIVE")'")
else
  ARCHIVE="$(dirname "$OUT")/$(basename "$OUT").tar.gz"
  tar -C "$(dirname "$OUT")" -czf "$ARCHIVE" "$(basename "$OUT")"
fi
du -sh "$OUT" "$ARCHIVE"

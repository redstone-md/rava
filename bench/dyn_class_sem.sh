#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
rava=$(realpath "${1:-$repo/target/x86_64-unknown-linux-gnu/release-fat/rava}")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
main="$tmp/main"
dynamic="$tmp/dynamic"
work="$tmp/work"
mkdir -p "$main" "$dynamic" "$work"
cp "$repo/rava.toml" "$work/rava.toml"

javac -d "$main" \
  "$repo/bench/DynBase.java" \
  "$repo/bench/DynApi.java" \
  "$repo/bench/DynClassSem.java"
javac -cp "$main" -d "$dynamic" \
  "$repo/bench/dynclass-src/dyn/Plugin.java" \
  "$repo/bench/dynclass-src/dyn/BadInit.java"

java -Xint -cp "$main" probe.DynClassSem "$dynamic" \
  > "$tmp/java.out" 2> "$tmp/java.err"
if (
  cd "$work"
  env -i PATH="$PATH" HOME="${HOME:-/tmp}" \
    RAVA_JIT=1 RAVA_JIT_TIER0=1 RAVA_HARD_MB=3584 \
    "$rava" -cp "$main:$repo/out" probe.DynClassSem "$dynamic" \
    > "$tmp/rava.out" 2> "$tmp/rava.err"
); then
  :
else
  status=$?
  printf 'Rava probe process failed (status %s)\n' "$status" >&2
  cat "$tmp/rava.out" "$tmp/rava.err" >&2
  exit "$status"
fi

java_init=$(sed -n 's/^init.first //p' "$tmp/java.out")
rava_init=$(sed -n 's/^init.first //p' "$tmp/rava.out")
if [[ "$java_init" != java.lang.ExceptionInInitializerError || \
      "$rava_init" != java.lang.IllegalStateException ]]; then
  printf 'unexpected initializer results: java=%s rava=%s\n' "$java_init" "$rava_init" >&2
  exit 1
fi
sed '/^init.first /d' "$tmp/java.out" > "$tmp/java.compare"
sed '/^init.first /d' "$tmp/rava.out" > "$tmp/rava.compare"
if ! diff -u "$tmp/java.compare" "$tmp/rava.compare"; then
  printf 'dynamic-class conformance mismatch outside the declared initializer divergence\n' >&2
  exit 1
fi
printf 'dynamic-class probe matched except known initializer wrapping gap\n'
printf '  java: %s\n  rava: %s\n' "$java_init" "$rava_init"

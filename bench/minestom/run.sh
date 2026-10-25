#!/bin/bash
# Boots Minestom (an open-source Minecraft server) on HotSpot and on rava and prints each
# phase: registry bootstrap, parallel chunk generation, network start.
# Usage: bench/minestom/run.sh [runs] [radius]
# Set JAVA_EXTRA for common HotSpot flags and JAVA_FALLBACK_EXTRA for a second HotSpot mode.
# The latter enables a three-way, order-rotated comparison with Rava.
# Needs Maven Central for the first dependency fetch.
set -eo pipefail
cd "$(dirname "$0")"
R=${R:-../../target/release/rava}
runs=${1:-3}
radius=${2:-8}
if ! [[ $runs =~ ^[1-9][0-9]*$ && $radius =~ ^[1-9][0-9]*$ ]]; then
  echo "runs and radius must be positive integers" >&2
  exit 2
fi
if (( radius > 64 )); then
  echo "radius must be at most 64" >&2
  exit 2
fi
read -r -a java_extra <<< "${JAVA_EXTRA:-}"
read -r -a java_fallback_extra <<< "${JAVA_FALLBACK_EXTRA:-}"
dependency_dir=../../target/minestom-lib
mkdir -p out "$dependency_dir"
minestom_version=$(sed -n 's/.*<artifactId>minestom<\/artifactId><version>\([^<]*\)<\/version>.*/\1/p' pom.xml)
if command -v mvn >/dev/null 2>&1; then
  mvn -q -B dependency:copy-dependencies -DoutputDirectory="$dependency_dir" -DincludeScope=runtime
elif [[ -n $minestom_version && -f "$dependency_dir/minestom-$minestom_version.jar" ]]; then
  echo "mvn unavailable; reusing cached Minestom dependencies" >&2
else
  echo "Maven is required to fetch Minestom $minestom_version runtime dependencies" >&2
  exit 1
fi
case $(uname -s) in
  MINGW*|MSYS*|CYGWIN*) sep=';' ;;
  *) sep=':' ;;
esac
jars=("$dependency_dir"/*.jar)
if [[ ! -f ${jars[0]} ]]; then
  echo "Maven did not produce runtime dependency jars in $dependency_dir" >&2
  exit 1
fi
CP=out
for jar in "${jars[@]}"; do CP+="$sep$jar"; done
mkdir -p out && javac -d out -cp "$CP" src/MsBench.java
expected_chunk_count=$((4 * radius * radius))
expected_hash=
run() {
  local vm=$1 output
  if [ "$vm" = java ]; then
    output=$(java "${java_extra[@]}" -cp "$CP" MsBench "$radius" | grep '^MSBENCH ')
  elif [ "$vm" = java-fallback ]; then
    output=$(java "${java_extra[@]}" "${java_fallback_extra[@]}" -cp "$CP" MsBench "$radius" | grep '^MSBENCH ')
  else
    output=$("$R" -cp "$CP" MsBench "$radius" | grep '^MSBENCH ')
  fi
  if [[ $output != *"radius=$radius chunk_count=$expected_chunk_count "* ]]; then
    echo "$vm produced unexpected radius or chunk count: $output" >&2
    return 1
  fi
  local hash
  hash=$(sed -n 's/.* hash=\([[:xdigit:]]*\)$/\1/p' <<< "$output")
  if [[ ! $hash =~ ^[[:xdigit:]]+$ ]]; then
    echo "$vm produced no valid result hash: $output" >&2
    return 1
  fi
  if [[ -z $expected_hash ]]; then
    expected_hash=$hash
  elif [[ $hash != "$expected_hash" ]]; then
    echo "$vm result hash $hash differs from expected $expected_hash" >&2
    return 1
  fi
  printf '%s: %s\n' "$vm" "$output"
}
shared_java_options=none
if [[ -n ${JAVA_TOOL_OPTIONS:-}${JDK_JAVA_OPTIONS:-}${_JAVA_OPTIONS:-} ]]; then
  shared_java_options=present
fi
echo "runs=$runs radius=$radius java_extra=${JAVA_EXTRA:-none} java_fallback_extra=${JAVA_FALLBACK_EXTRA:-none} shared_java_options=$shared_java_options"
for i in $(seq "$runs"); do
  if (( ${#java_fallback_extra[@]} > 0 )); then
    case $(( (i - 1) % 5 )) in
      0) order=(java java-fallback rava) ;;
      1) order=(java-fallback rava java) ;;
      2) order=(rava java java-fallback) ;;
      3) order=(java-fallback java rava) ;;
      4) order=(rava java-fallback java) ;;
    esac
    for vm in "${order[@]}"; do run "$vm"; done
  elif (( i % 2 )); then
    run java
    run rava
  else
    run rava
    run java
  fi
done

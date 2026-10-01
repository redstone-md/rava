#!/bin/bash
# Paper server start A/B: alternates builds on one host with a fixed Paper version, seed and a
# fresh world per run, and keeps every run (not just a summary).
#
#   bench/paper_ab.sh [-n ROUNDS] [-b PAPER_BASE] [-o OUT] REV|java ...
#
# REV is any git revision of this repository: it is built once (release profile) in its own
# worktree under $AB_CACHE, so each result names the exact commit and binary. `java` runs the
# same server on $JAVA_HOME's JVM. Each round runs every build once, in the given order.
#
# PAPER_BASE is a directory with the server set up once (paper jar, eula.txt, libraries/ and
# versions/ unpacked by a first run, server.properties with a fixed level-seed); each run copies
# it into a scratch directory, so every world is generated from scratch.
#
# OUT (default bench/results/<utc date>-<host>) gets:
#   env.txt   host, CPU, kernel, toolchains, Paper jar and server.properties hashes, seed, the
#             RAVA_*/JAVA_* environment, the repository head;
#   runs.tsv  one row per run: time, round, label, commit, binary sha256, wall seconds from exec
#             to the "Done (" line, the server's own Done time, user/sys CPU, milestone offsets;
#   logs/     the server log of every run (gzip).
set -euo pipefail
ROUNDS=3
BASE=${PAPER_BASE:-/tmp/rava/paper-base}
OUT=""
while getopts n:b:o: opt; do
  case $opt in
    n) ROUNDS=$OPTARG ;;
    b) BASE=$OPTARG ;;
    o) OUT=$OPTARG ;;
    *) sed -n '2,20p' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
[ $# -ge 1 ] || { sed -n '2,20p' "$0"; exit 2; }
REPO=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${AB_CACHE:-/tmp/rava-ab-cache}
JAVA=${JAVA_HOME:-/usr/lib/jvm/java-21-openjdk-amd64}/bin/java
JAR=$(cd "$BASE" && ls paper-*.jar | head -1)
OUT=${OUT:-$REPO/bench/results/$(date -u +%Y%m%d-%H%M%S)-$(hostname -s)}
mkdir -p "$OUT/logs" "$CACHE"

# Builds (or reuses) each revision; label -> command, commit, binary hash.
declare -A CMD COMMIT SHA
for rev in "$@"; do
  if [ "$rev" = java ]; then
    CMD[$rev]="$JAVA"; COMMIT[$rev]=-; SHA[$rev]=$(sha256sum "$(readlink -f "$JAVA")" | cut -c1-16)
    continue
  fi
  c=$(git -C "$REPO" rev-parse --verify "$rev^{commit}")
  wt=$CACHE/$c
  if [ ! -x "$wt/target/release/rava" ]; then
    [ -d "$wt" ] || git -C "$REPO" worktree add --detach -f "$wt" "$c" >/dev/null
    (cd "$wt" && cargo build --release -p rava >/dev/null 2>&1)
  fi
  CMD[$rev]=$wt/target/release/rava; COMMIT[$rev]=$c; SHA[$rev]=$(sha256sum "${CMD[$rev]}" | cut -c1-16)
done

{
  echo "date_utc $(date -u +%FT%TZ)"
  echo "host $(hostname) nproc $(nproc) mem_kb $(awk '/MemTotal/{print $2}' /proc/meminfo)"
  echo "cpu $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')"
  echo "kernel $(uname -r)"
  echo "rustc $(rustc --version 2>/dev/null || echo -)"
  echo "java $("$JAVA" -version 2>&1 | grep -v '^Picked up' | head -1)"
  echo "paper_jar $JAR $(sha256sum "$BASE/$JAR" | cut -d' ' -f1)"
  echo "server_properties $(tr -d '\r' < "$BASE/server.properties" | sha256sum | cut -d' ' -f1) $(tr -d '\r' < "$BASE/server.properties" | grep -E '^(level-seed|level-name|view-distance|simulation-distance)=' | tr '\n' ' ')"
  echo "repo_head $(git -C "$REPO" rev-parse HEAD)"
  for rev in "$@"; do echo "build $rev commit ${COMMIT[$rev]} sha256 ${SHA[$rev]} cmd ${CMD[$rev]}"; done
  env | grep -E '^(RAVA_|JAVA_HOME)' | sort | sed 's/^/env /' || true
  # The sandbox's JAVA_TOOL_OPTIONS hold its proxy and trust-store settings: noted, not copied.
  [ -n "${JAVA_TOOL_OPTIONS:-}" ] && echo "env JAVA_TOOL_OPTIONS set (sandbox proxy/trust-store options, not recorded)"
  true
} > "$OUT/env.txt"
[ -f "$OUT/runs.tsv" ] || printf 'time_utc\tround\tlabel\tcommit\tbin_sha256\twall_s\tdone_s\tuser_s\tsys_s\tmilestones\n' > "$OUT/runs.tsv"

run_one() { # label round
  local label=$1 round=$2 d
  d=$(mktemp -d /tmp/paper-ab-run.XXXXXX)
  cp -r "$BASE/." "$d/"
  rm -rf "$d"/world* "$d/logs"
  (cd "$d" && mkfifo in.fifo)
  local start end pid
  start=$(date +%s.%N)
  (cd "$d" && exec ${CMD[$label]} -jar "$JAR" --nogui < in.fifo > out.txt 2>&1) &
  pid=$!
  exec 3> "$d/in.fifo"
  # A server that fails (or hangs) before "Done" is recorded as FAILED after RUN_LIMIT seconds.
  local limit=$(( $(date +%s) + ${RUN_LIMIT:-300} ))
  while ! grep -q 'Done (' "$d/out.txt"; do
    kill -0 $pid 2>/dev/null || break
    [ "$(date +%s)" -lt $limit ] || break
    sleep 0.1
  done
  end=$(date +%s.%N)
  local ut=0 st=0 tck
  tck=$(getconf CLK_TCK)
  if [ -r /proc/$pid/stat ]; then read -r ut st < <(awk '{print $14, $15}' /proc/$pid/stat); fi
  echo stop >&3
  exec 3>&-
  for _ in $(seq 1 240); do kill -0 $pid 2>/dev/null || break; sleep 0.25; done
  kill -9 $pid 2>/dev/null || true
  wait $pid 2>/dev/null || true
  local wall done_s ms
  if grep -q 'Done (' "$d/out.txt"; then wall=$(echo "$end - $start" | bc); else wall=FAILED; fi
  done_s=$(grep -o 'Done ([0-9.]*s)' "$d/out.txt" | grep -o '[0-9.]*' | head -1 || true)
  ms=$(python3 - "$d/out.txt" "$start" <<'PY'
import sys, re, datetime
lines = open(sys.argv[1], errors='replace').read().splitlines()
st = datetime.datetime.fromtimestamp(float(sys.argv[2]))
base = st.hour * 3600 + st.minute * 60 + st.second + st.microsecond / 1e6
marks = [("bootstrap", "Loading Paper"), ("env", "Environment:"), ("prep", "Preparing level"),
         ("spawn", "Preparing start region for dimension minecraft:overworld"),
         ("nether", "minecraft:the_nether"), ("end", "minecraft:the_end"), ("done", "Done (")]
out = []
for key, pat in marks:
    for l in lines:
        m = re.match(r'\[(\d+):(\d+):(\d+)', l)
        if m and pat in l:
            out.append(f"{key}@{int(m[1])*3600 + int(m[2])*60 + int(m[3]) - base:.0f}")
            break
print(",".join(out))
PY
)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$round" "$label" "${COMMIT[$label]}" "${SHA[$label]}" \
    "$wall" "${done_s:--}" "$(echo "scale=1; $ut/$tck" | bc)" "$(echo "scale=1; $st/$tck" | bc)" "$ms" >> "$OUT/runs.tsv"
  gzip -c "$d/out.txt" > "$OUT/logs/r${round}-${label//\//_}.log.gz"
  rm -rf "$d"
  echo "round $round $label wall $wall done ${done_s:--}"
}

for round in $(seq 1 "$ROUNDS"); do
  for label in "$@"; do run_one "$label" "$round"; done
done
echo "results: $OUT"

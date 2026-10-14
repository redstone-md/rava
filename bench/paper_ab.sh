#!/bin/bash
# Paper server start A/B: alternates builds on one host with a fixed Paper version, seed and a
# fresh world per run, and keeps every run (not just a summary).
#
#   bench/paper_ab.sh [-n ROUNDS] [-b PAPER_BASE] [-o OUT] [-m SOURCE_MANIFEST]
#                     REV|java|binary:/absolute/path ...
#
# REV is any git revision of this repository: it is built once (release profile) in its own
# worktree under $AB_CACHE, so each result names the exact commit and binary. `java` runs the
# same server on $JAVA_HOME's JVM. Each round runs every build once, in the given order.
# `binary:/absolute/path` runs an existing executable without building or committing it;
# its commit is recorded as `-`, with its full SHA256 and resolved path. This can validate
# an uncommitted compiler change. env.txt records the checkout's source base and diff hashes,
# not a claim about a prebuilt binary's source; -m preserves a supplied build/source manifest.
#
# PAPER_BASE is a directory with the server set up once (paper jar, eula.txt, libraries/ and
# versions/ unpacked by a first run, server.properties with a fixed level-seed); each run copies
# it into a scratch directory, so every world is generated from scratch. A base with a
# `launch.args` file (one line, e.g. a NeoForge server's `@user_jvm_args.txt @libraries/...
# nogui`) is started with those arguments instead of `-jar paper-*.jar --nogui`.
#
# OUT (default bench/results/<utc date>-<host>) gets:
#   env.txt   host, CPU, kernel, toolchains, Paper jar and server.properties hashes, seed, the
#             RAVA_*/JAVA_* environment, the repository head;
#   runs.tsv  one row per run: time, round, label, commit, binary sha256, wall seconds from exec
#             to the "Done (" line, the server's own Done time, user/sys CPU, peak and current
#             resident memory (MiB) at Done, milestone offsets;
#   logs/     the server log of every run (gzip).
#   source.diff, source-untracked.tsv   checkout changes and untracked file hashes at launch;
#   source-manifest   an optional supplied manifest, copied verbatim.
set -euo pipefail
trap '' PIPE
ROUNDS=3
BASE=${PAPER_BASE:-${XDG_CACHE_HOME:-$HOME/.cache}/rava/paper-base}
OUT=""
MANIFEST=""
while getopts n:b:o:m: opt; do
  case $opt in
    n) ROUNDS=$OPTARG ;;
    b) BASE=$OPTARG ;;
    o) OUT=$OPTARG ;;
    m) MANIFEST=$OPTARG ;;
    *) sed -n '2,20p' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
[ $# -ge 1 ] || { sed -n '2,20p' "$0"; exit 2; }
REPO=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${AB_CACHE:-/tmp/rava-ab-cache}
JAVA=${JAVA_HOME:-/usr/lib/jvm/java-21-openjdk-amd64}/bin/java
JAR=$(cd "$BASE" && ls paper-*.jar 2>/dev/null | head -1 || true)
if [ -f "$BASE/launch.args" ]; then ARGS=$(cat "$BASE/launch.args"); else ARGS="-jar $JAR --nogui"; fi
OUT=${OUT:-$REPO/bench/results/$(date -u +%Y%m%d-%H%M%S)-$(hostname -s)}
mkdir -p "$OUT/logs"
file_sha256() { sha256sum < "$1" | cut -d' ' -f1; }
if [ -n "$MANIFEST" ]; then
  [ -f "$MANIFEST" ] || { echo "source manifest is not a file: $MANIFEST" >&2; exit 2; }
  cp "$MANIFEST" "$OUT/source-manifest"
fi
SOURCE_BASE=$(git -C "$REPO" rev-parse HEAD)
git -C "$REPO" diff --binary HEAD -- > "$OUT/source.diff"
# git diff excludes untracked probes. Keep their hashes too, without treating them as built.
: > "$OUT/source-untracked.tsv"
git -C "$REPO" ls-files --others --exclude-standard -z | sort -z > "$OUT/source-untracked.paths"
while IFS= read -r -d '' file; do
  printf '%s\t%s\n' "$(file_sha256 "$REPO/$file")" "$file" >> "$OUT/source-untracked.tsv"
done < "$OUT/source-untracked.paths"
rm "$OUT/source-untracked.paths"

# Builds (or reuses) each revision; label -> command, commit, binary hash.
declare -A CMD COMMIT SHA
for rev in "$@"; do
  if [ "$rev" = java ]; then
    CMD[$rev]="$JAVA"; COMMIT[$rev]=-; SHA[$rev]=$(file_sha256 "$(readlink -f "$JAVA")")
    continue
  fi
  if [[ "$rev" == binary:* ]]; then
    bin=${rev#binary:}
    [[ "$bin" == /* && -f "$bin" && -x "$bin" ]] || {
      echo "binary requires an absolute path to an executable file: $bin" >&2; exit 2;
    }
    CMD[$rev]=$(readlink -f "$bin"); COMMIT[$rev]=-; SHA[$rev]=$(file_sha256 "${CMD[$rev]}")
    continue
  fi
  c=$(git -C "$REPO" rev-parse --verify "$rev^{commit}")
  mkdir -p "$CACHE"
  wt=$CACHE/$c
  # Revisions before the rename build the binary as rava-core.
  bin=$wt/target/release/rava; [ -x "$bin" ] || bin=$wt/target/release/rava-core
  if [ ! -x "$bin" ]; then
    [ -d "$wt" ] || git -C "$REPO" worktree add --detach -f "$wt" "$c" >/dev/null
    (cd "$wt" && cargo build --release >/dev/null 2>&1)
    bin=$wt/target/release/rava; [ -x "$bin" ] || bin=$wt/target/release/rava-core
  fi
  CMD[$rev]=$bin; COMMIT[$rev]=$c; SHA[$rev]=$(file_sha256 "$bin")
done

{
  echo "date_utc $(date -u +%FT%TZ)"
  echo "host $(hostname) nproc $(nproc) mem_kb $(awk '/MemTotal/{print $2}' /proc/meminfo 2>/dev/null || echo -)"
  cpu=$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ //' || true)
  echo "cpu ${cpu:--}"
  echo "kernel $(uname -r)"
  echo "rustc $(rustc --version 2>/dev/null || echo -)"
  echo "java $("$JAVA" -version 2>&1 | grep -v '^Picked up' | head -1)"
  if [ -n "$JAR" ]; then echo "paper_jar $JAR $(file_sha256 "$BASE/$JAR")"; fi
  echo "args $ARGS"
  echo "server_properties $(tr -d '\r' < "$BASE/server.properties" | sha256sum | cut -d' ' -f1) $(tr -d '\r' < "$BASE/server.properties" | grep -E '^(level-seed|level-name|view-distance|simulation-distance)=' | tr '\n' ' ')"
  echo "repo_head $SOURCE_BASE"
  echo "source_base $SOURCE_BASE"
  echo "source_diff_sha256 $(file_sha256 "$OUT/source.diff")"
  echo "source_untracked_sha256 $(file_sha256 "$OUT/source-untracked.tsv")"
  if [ -n "$MANIFEST" ]; then echo "source_manifest_sha256 $(file_sha256 "$OUT/source-manifest")"; fi
  for rev in "$@"; do echo "build $rev commit ${COMMIT[$rev]} sha256 ${SHA[$rev]} cmd ${CMD[$rev]}"; done
  env | grep -E '^(RAVA_|JAVA_HOME)' | sort | sed 's/^/env /' || true
  # The sandbox's JAVA_TOOL_OPTIONS hold its proxy and trust-store settings: noted, not copied.
  [ -n "${JAVA_TOOL_OPTIONS:-}" ] && echo "env JAVA_TOOL_OPTIONS set (sandbox proxy/trust-store options, not recorded)"
  true
} > "$OUT/env.txt"
[ -f "$OUT/runs.tsv" ] || printf 'time_utc\tround\tlabel\tcommit\tbin_sha256\twall_s\tdone_s\tuser_s\tsys_s\thwm_mb\trss_mb\tmilestones\n' > "$OUT/runs.tsv"

run_one() { # label round
  local label=$1 round=$2 d
  d=$(mktemp -d "${TMPDIR:-$OUT}/paper-ab-run.XXXXXX")
  cp -r "$BASE/." "$d/"
  rm -rf "$d"/world* "$d/logs" "$d/crash-reports"
  (cd "$d" && mkfifo in.fifo)
  local start end pid
  start=$(date +%s.%N)
  # shellcheck disable=SC2086 # ARGS is a word list
  (cd "$d" && exec "${CMD[$label]}" $ARGS < in.fifo > out.txt 2>&1) &
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
  local ut=- st=- tck
  tck=$(getconf CLK_TCK)
  local hwm=- rss=-
  # comm is parenthesized and may contain spaces, including a prebuilt binary's file name.
  local procstat
  procstat=$(cat /proc/$pid/stat 2>/dev/null || true)
  if [ -n "$procstat" ]; then
    procstat=${procstat##*) }
    read -r ut st <<< "$(awk '{print $12, $13}' <<< "$procstat")"
  fi
  if [ -r /proc/$pid/status ]; then
    hwm=$(awk '/^VmHWM/{printf "%.0f", $2/1024}' /proc/$pid/status)
    rss=$(awk '/^VmRSS/{printf "%.0f", $2/1024}' /proc/$pid/status)
  fi
  # A server that died already has no reader on the FIFO: the write fails (EPIPE, as SIGPIPE
  # is ignored below) and the run is recorded as FAILED instead of ending the whole A/B.
  echo stop >&3 2>/dev/null || true
  exec 3>&-
  for _ in $(seq 1 240); do kill -0 $pid 2>/dev/null || break; sleep 0.25; done
  kill -9 $pid 2>/dev/null || true
  wait $pid 2>/dev/null || true
  local wall done_s ms
  if grep -q 'Done (' "$d/out.txt"; then wall=$(python3 -c 'import sys; print(float(sys.argv[2]) - float(sys.argv[1]))' "$start" "$end"); else wall=FAILED; fi
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
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$round" "$label" "${COMMIT[$label]}" "${SHA[$label]}" \
    "$wall" "${done_s:--}" "$(python3 -c 'import sys; print("-" if sys.argv[1] == "-" else f"{int(sys.argv[1]) / int(sys.argv[2]):.1f}")' "$ut" "$tck")" "$(python3 -c 'import sys; print("-" if sys.argv[1] == "-" else f"{int(sys.argv[1]) / int(sys.argv[2]):.1f}")' "$st" "$tck")" "$hwm" "$rss" "$ms" >> "$OUT/runs.tsv"
  gzip -c "$d/out.txt" > "$OUT/logs/r${round}-${label//\//_}.log.gz"
  rm -rf "$d"
  echo "round $round $label wall $wall done ${done_s:--} hwm ${hwm}M"
}

# Alternate label order every round so no variant is systematically penalised
# by first-position cold-cache bias (the first boot in a round is slower than
# later ones). Even rounds run the labels reversed.
labels=("$@")
for round in $(seq 1 "$ROUNDS"); do
  if (( round % 2 == 0 )); then
    for (( i = ${#labels[@]} - 1; i >= 0; i-- )); do run_one "${labels[$i]}" "$round"; done
  else
    for label in "${labels[@]}"; do run_one "$label" "$round"; done
  fi
done
echo "results: $OUT"

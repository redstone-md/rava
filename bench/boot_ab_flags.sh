#!/usr/bin/env bash
# Interleaved startup A/B for flags or whole binaries — Linux port of `boot_ab_flags.ps1`.
# carrying the three mitigations the null test forced on Windows and the reasons for each:
#
#   * a DISCARDED warm-up boot, because the first boot of a process pays page-cache and
#     file-open costs no later boot repeats;
#   * ALTERNATING order (A B B A per pair), because a sweep whose arms run in sequence measures
#     WHEN they ran — that mistake produced two false findings in one hour on 2026-09-03;
#   * the MEDIAN reported beside the min, because one slow boot moves a min and not a median.
#
# It also DROPS a round whose arm never printed `Done`: a boot that faulted is not a data point,
# and averaging a corpse as a fast boot is how this harness lied before.
#
# The reported wall is launch-to-Done (`boot`), clocked at the instant `Done` appears in
# the log; the teardown-inclusive `kill_wall` survives only as a secondary diagnostic line.
#
# Usage: bench/boot_ab_flags.sh "FLAG1,FLAG2" [rounds] [timeout_s]
# Use "-" for whole-binary A/B without environment flags. Optional executable paths
# RAVA_BOOT_EXE_OFF / RAVA_BOOT_EXE_ON are resolved relative to the repo root.
set -u
FLAGS_CSV="${1:?usage: boot_ab_flags.sh FLAG[,FLAG...]|- [rounds] [timeout_s]}"
ROUNDS="${2:-4}"
TIMEOUT="${3:-400}"
cd "$(dirname "$0")/.."

EXE_DEFAULT="target/release/rava"
# Whole-binary A/B avoids perturbing the child with an in-process test flag.
EXE_OFF="$(realpath -- "${RAVA_BOOT_EXE_OFF:-$EXE_DEFAULT}")"
EXE_ON="$(realpath -- "${RAVA_BOOT_EXE_ON:-$EXE_DEFAULT}")"
[ -x "$EXE_OFF" ] || { echo "missing OFF executable: $EXE_OFF"; exit 1; }
[ -x "$EXE_ON" ] || { echo "missing ON executable: $EXE_ON"; exit 1; }
CP="$(cat bench/mc-cp.txt | tr -d '\r\n')"

if [ "$FLAGS_CSV" != "-" ]; then IFS=',' read -ra FLAGS <<< "$FLAGS_CSV"; else FLAGS=(); fi

# One boot. $1 = "ON"|"OFF". Echoes "boot cpu done kill_wall" or nothing when the boot never
# reached Done. `boot` is the launch-to-Done wall; `kill_wall` additionally includes teardown
# and is a diagnostic only.
boot() {
  local arm="$1" log="/tmp/bootab_$1.log"
  local exe="$EXE_OFF"
  [ "$arm" = "ON" ] && exe="$EXE_ON"
  # A failed world reset is not a data point either: measuring against a stale `world` is how
  # a corrupted round reads as a clean one. Fail the arm; the caller drops the round.
  # Copy directory contents through `/.` so an ignored top-level symlink is followed as a source,
  # never recreated as `world` and allowed to alias/mutate the pristine fixture.
  rm -rf -- world && mkdir -- world && cp -r -- world_pristine/. world ||
    { echo "world reset failed ($arm arm)" >&2; return 1; }
  # No RAVA_PROC_CAP_MB: the OS-enforced cap is a Windows Job Object; on Linux it only
  # prints a WARNING. RAVA_HARD_MB (allocator-level ceiling) works on both.
  # Start from a small host-environment allowlist. This drops every inherited RAVA_* knob
  # (including unknown future flags), plus LD_PRELOAD and other ambient process overrides.
  local launch=(env -i
    "PATH=${PATH:-/usr/bin:/bin}"
    "HOME=${HOME:-/tmp}"
    "TMPDIR=${TMPDIR:-/tmp}"
    "LANG=${LANG:-C.UTF-8}"
    "USER=${USER:-}"
    "LOGNAME=${LOGNAME:-}"
  )
  # Only the fixed product baseline and explicitly requested ON-arm flags enter the child.
  # Environment isolation does not certify source-level no-adapt; runtime name/count gates
  # still require their own audit.
  launch+=(RAVA_JIT=1 RAVA_JIT_TIER0=1 RAVA_HARD_MB=3584)
  if [ "$arm" = "ON" ]; then
    for f in "${FLAGS[@]}"; do launch+=("$f=1"); done
  fi
  local t0 t1 t_done boot kill_wall cpu done_s pid deadline ticks
  ticks=$(getconf CLK_TCK 2>/dev/null || echo 100)
  t0=$(date +%s.%N)
  # The server never exits on its own, so waiting on it (or on `timeout`) would hang every
  # round until TIMEOUT. Poll for `Done (` and kill ONLY the exact PID started here — never
  # java by name (the user's own server may share this box).
  "${launch[@]}" "$exe" -cp "$CP" net.minecraft.server.Main --nogui > "$log" 2>&1 &
  pid=$!
  deadline=$((SECONDS + TIMEOUT))
  done_s=""
  t_done=""
  # Poll at 0.2s, not 1s, so detection latency adds at most 0.2s to the reported boot wall.
  while [ $SECONDS -lt $deadline ]; do
    kill -0 "$pid" 2>/dev/null || break
    if grep -qE 'Done \([0-9.]+s\)' "$log" 2>/dev/null; then
      t_done=$(date +%s.%N)
      done_s=$(grep -oE 'Done \([0-9.]+s\)' "$log" | tail -1 | grep -oE '[0-9.]+')
      break
    fi
    sleep 0.2
  done
  # CPU seconds of the exact PID from /proc (utime+stime ticks) — the figure the Windows
  # harness takes from TotalProcessorTime. Read BEFORE the kill, while /proc still has it.
  cpu=""
  if [ -r "/proc/$pid/stat" ]; then
    cpu=$(awk -v t="$ticks" '{printf "%.1f", ($14+$15)/t}' "/proc/$pid/stat" 2>/dev/null)
  fi
  if kill -0 "$pid" 2>/dev/null; then kill "$pid" 2>/dev/null; sleep 2; kill -9 "$pid" 2>/dev/null; fi
  wait "$pid" 2>/dev/null
  t1=$(date +%s.%N)
  # The reported wall stops at DETECTION: `boot` clocks launch to the instant `Done` was
  # seen, not to the demolition above. `kill_wall` keeps the teardown-inclusive figure
  # (TERM, 2s grace, KILL, wait) as a diagnostic only.
  [ -n "$done_s" ] || return 1
  boot=$(awk -v a="$t0" -v b="$t_done" 'BEGIN{printf "%.1f", b-a}')
  kill_wall=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.1f", b-a}')
  echo "$boot ${cpu:-0} $done_s $kill_wall"
}

echo "boot A/B  flags=${FLAGS_CSV}  rounds=${ROUNDS}"
echo "  warm-up boot (discarded)..."
boot OFF > /dev/null 2>&1 || true

on_cpu=(); off_cpu=(); on_boot=(); off_boot=(); on_kill_wall=(); off_kill_wall=()
for ((r=1; r<=ROUNDS; r++)); do
  # ABBA: the arm that runs first alternates, so a monotonic drift cancels instead of
  # accumulating into whichever arm is always last.
  if (( r % 2 == 1 )); then order=(OFF ON); else order=(ON OFF); fi
  a=""; b=""
  for arm in "${order[@]}"; do
    res=$(boot "$arm") || { echo "  round $r: $arm never reached Done — round DROPPED"; a=""; break; }
    if [ "$arm" = "ON" ]; then a="$res"; else b="$res"; fi
  done
  [ -n "$a" ] && [ -n "$b" ] || continue
  read -r ob oc od okw <<< "$a"; read -r fb fc fd fkw <<< "$b"
  echo "  round $r: OFF boot=$fb cpu=$fc Done=$fd   ON boot=$ob cpu=$oc Done=$od"
  on_cpu+=("$oc"); off_cpu+=("$fc"); on_boot+=("$ob"); off_boot+=("$fb")
  on_kill_wall+=("$okw"); off_kill_wall+=("$fkw")
done

stat() { printf '%s\n' "$@" | sort -n | awk '{v[NR]=$1} END{ if(NR==0){print "0 0"; exit} m=(NR%2)?v[(NR+1)/2]:(v[NR/2]+v[NR/2+1])/2; printf "%.2f %.2f", v[1], m }'; }
[ "${#on_cpu[@]}" -gt 0 ] || { echo "no usable rounds"; exit 1; }
read -r on_min on_med <<< "$(stat "${on_cpu[@]}")"
read -r off_min off_med <<< "$(stat "${off_cpu[@]}")"
awk -v a="$on_min" -v b="$off_min" -v c="$on_med" -v d="$off_med" \
  'BEGIN{printf "CPU-s: ON min=%.1f OFF min=%.1f delta=%.2f%% | ON median=%.2f OFF median=%.2f delta=%.2f%%\n", a,b,(a-b)/b*100, c,d,(c-d)/d*100}'
read -r onb_min onb_med <<< "$(stat "${on_boot[@]}")"
read -r offb_min offb_med <<< "$(stat "${off_boot[@]}")"
awk -v a="$onb_min" -v b="$offb_min" -v c="$onb_med" -v d="$offb_med" \
  'BEGIN{printf "boot-s: ON min=%.1f OFF min=%.1f delta=%.2f%% | ON median=%.2f OFF median=%.2f delta=%.2f%%\n", a,b,(a-b)/b*100, c,d,(c-d)/d*100}'
echo "kill_wall (teardown-inclusive, diagnostic only): ON ${on_kill_wall[*]}  OFF ${off_kill_wall[*]}"

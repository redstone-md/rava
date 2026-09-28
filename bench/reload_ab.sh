#!/usr/bin/env bash
# Interleaved same-binary `ReloadBench` A/B — the Linux port of `reload_ab.ps1`.
#
# `RELOAD2_MS` is the metric to price on: `RELOAD_MS` carries the whole first-pass JIT warm-up,
# while the second reload over the same `ResourceManager` is steady-state execution. The content
# HASH is checked on every run and a mismatch aborts — a faster arm that computed something else
# is not a result.
#
# Usage: bench/reload_ab.sh FLAG [pairs]
set -u
FLAG="${1:?usage: reload_ab.sh FLAG [pairs]}"
PAIRS="${2:-3}"
cd "$(dirname "$0")/.."

EXE="target/release/rava"
[ -x "$EXE" ] || { echo "missing $EXE"; exit 1; }
MCCP="$(cat bench/mc-cp.txt | tr -d '\r\n')"
CP="out:$MCCP"
WANT_HASH="c956377e19071d7a"

mkdir -p out
javac -nowarn -cp "$MCCP" -d out bench/ReloadBench.java 2>/dev/null
[ -f out/ReloadBench.class ] || { echo "javac failed"; exit 1; }

run() { # $1 = ON|OFF  -> "reload_ms reload2_ms cpu_ms"
  local arm="$1"
  local log="/tmp/reloadab_$arm.log"
  local env_prefix=(env -i
    "PATH=${PATH:-/usr/bin:/bin}"
    "HOME=${HOME:-/tmp}"
    "TMPDIR=${TMPDIR:-/tmp}"
    "LANG=${LANG:-C.UTF-8}"
    "USER=${USER:-}"
    "LOGNAME=${LOGNAME:-}"
    RAVA_JIT=1 RAVA_JIT_TIER0=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584
  )
  [ "$arm" = "ON" ] && env_prefix+=("$FLAG=1")
  /usr/bin/time -f "%U %S" -o /tmp/reloadab_t_$arm \
    "${env_prefix[@]}" "$EXE" -cp "$CP" ReloadBench --reloads 2 > "$log" 2>/dev/null || return 1
  local line hash
  line=$(grep -m1 'RELOAD_MS=' "$log") || return 1
  hash=$(sed -n 's/.*HASH=\([0-9a-f]*\).*/\1/p' <<< "$line")
  [ "$hash" = "$WANT_HASH" ] || { echo "HASH MISMATCH in $arm: $hash != $WANT_HASH" >&2; return 1; }
  local r1 r2 cpu
  r1=$(sed -n 's/.*RELOAD_MS=\([0-9]*\).*/\1/p' <<< "$line")
  r2=$(sed -n 's/.*RELOAD2_MS=\([0-9]*\).*/\1/p' <<< "$line")
  cpu=$(awk '{printf "%.0f", ($1+$2)*1000}' /tmp/reloadab_t_$arm)
  echo "$r1 $r2 $cpu"
}

echo "ReloadBench A/B  flag=$FLAG  pairs=$PAIRS"
run OFF > /dev/null 2>&1 || true   # discarded warm-up

a_r2=(); b_r2=(); a_cpu=(); b_cpu=()
for ((p=1; p<=PAIRS; p++)); do
  if (( p % 2 == 1 )); then order=(OFF ON); else order=(ON OFF); fi
  A=""; B=""
  for arm in "${order[@]}"; do
    res=$(run "$arm") || { echo "  pair $p: $arm failed — DROPPED"; A=""; break; }
    [ "$arm" = "ON" ] && B="$res" || A="$res"
  done
  [ -n "$A" ] && [ -n "$B" ] || continue
  read -r ar1 ar2 acpu <<< "$A"; read -r br1 br2 bcpu <<< "$B"
  echo "  pair $p: OFF RELOAD=$ar1 RELOAD2=$ar2 cpu=$acpu   ON RELOAD=$br1 RELOAD2=$br2 cpu=$bcpu"
  a_r2+=("$ar2"); b_r2+=("$br2"); a_cpu+=("$acpu"); b_cpu+=("$bcpu")
done

report() { # $1=name, then A-values..., "--", B-values...
  local name="$1"; shift
  local -a A=() B=(); local seen=0
  for v in "$@"; do [ "$v" = "--" ] && { seen=1; continue; }; [ $seen -eq 0 ] && A+=("$v") || B+=("$v"); done
  [ "${#A[@]}" -gt 0 ] || return
  local amin amed bmin bmed
  read -r amin amed <<< "$(printf '%s\n' "${A[@]}" | sort -n | awk '{v[NR]=$1} END{m=(NR%2)?v[(NR+1)/2]:(v[NR/2]+v[NR/2+1])/2; printf "%.2f %.2f", v[1], m}')"
  read -r bmin bmed <<< "$(printf '%s\n' "${B[@]}" | sort -n | awk '{v[NR]=$1} END{m=(NR%2)?v[(NR+1)/2]:(v[NR/2]+v[NR/2+1])/2; printf "%.2f %.2f", v[1], m}')"
  # DISJOINT means every B below every A — the only verdict this box's noise cannot manufacture.
  local amax bmax dis="arms overlap"
  amax=$(printf '%s\n' "${A[@]}" | sort -n | tail -1)
  bmax=$(printf '%s\n' "${B[@]}" | sort -n | tail -1)
  awk -v bm="$bmax" -v am="$amin" 'BEGIN{exit !(bm < am)}' && dis="every B below every A (DISJOINT, flag faster)"
  awk -v am="$amax" -v bm="$bmin" 'BEGIN{exit !(am < bm)}' && dis="every A below every B (DISJOINT, flag SLOWER)"
  awk -v n="$name" -v a="$amin" -v b="$bmin" -v c="$amed" -v d="$bmed" -v s="$dis" \
    'BEGIN{printf "  %-12s A %8.0f/%8.0f   B %8.0f/%8.0f   min %+.2f%%  median %+.2f%%   %s\n", n, a, c, b, d, (b-a)/a*100, (d-c)/c*100, s}'
}
echo "  metric        A min/median        B min/median"
report RELOAD2_MS "${a_r2[@]}" "--" "${b_r2[@]}"
report cpu_ms "${a_cpu[@]}" "--" "${b_cpu[@]}"

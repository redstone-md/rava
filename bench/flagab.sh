#!/usr/bin/env bash
# ABBA same-binary flag A/B on any self-timing bench. A = flag SET, B = flag UNSET.
#
# ⚠**Same binary on both arms is the strongest form this tree has**, and it is used here
# because a TWO-BINARY comparison of the same change read -16.88% (disjoint) and +30.72%
# hours apart: the identical baseline binary measured 11.59-11.93 s in one series and
# 8.87-12.42 s in the next, so the box's drift between series exceeded the effect. Only an
# interleaved same-binary series cancels that.
#   bench/flagab.sh RAVA_NO_CALL_PLAN Allocs 'HOTNS=([0-9]*)' 6
set -u
FLAG="$1"; PROG="$2"; PAT="${3:-HOTNS=\([0-9]*\)}"; PAIRS="${4:-6}"
export RAVA_JIT=0 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584
A=(); B=()
grab() { # $1 = "set" | "unset"
  if [ "$1" = set ]; then env "$FLAG=1" ./target/release/rava.exe -cp out "$PROG" 2>/dev/null | tr -d '\r' | sed -n "s/.*$PAT.*/\1/p" | head -1
  else ./target/release/rava.exe -cp out "$PROG" 2>/dev/null | tr -d '\r' | sed -n "s/.*$PAT.*/\1/p" | head -1; fi
}
for i in $(seq 1 "$PAIRS"); do
  if [ $((i % 2)) -eq 1 ]; then A+=($(grab set)); B+=($(grab unset)); else B+=($(grab unset)); A+=($(grab set)); fi
done
mn() { printf '%s\n' "$@" | sort -n | head -1; }
mx() { printf '%s\n' "$@" | sort -n | tail -1; }
am=$(mn "${A[@]}"); bm=$(mn "${B[@]}"); ax=$(mx "${A[@]}"); bx=$(mx "${B[@]}")
echo "$PROG   A=[$FLAG=1]  B=[unset]"
echo "  A ${A[*]}   min=$am"
echo "  B ${B[*]}   min=$bm"
if [ "$ax" -lt "$bm" ]; then v="every A below every B (DISJOINT)";
elif [ "$am" -gt "$bx" ]; then v="every A above every B (DISJOINT, opposite)";
else v="arms overlap"; fi
echo "  A vs B: $(awk -v a="$am" -v b="$bm" 'BEGIN{printf "%.2f%%", (a-b)*100/b}')   $v"

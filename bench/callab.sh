#!/usr/bin/env bash
# ABBA paired A/B on one CallCost row. A = env given in $1 (the arm under test), B = baseline.
# min-of-N per arm, disjointness reported. Between-process drift on this box moves the
# absolute level by tens of percent, which is why the arms alternate and only the minimum
# of each is compared -- an average here would be an average of contamination.
#   bench/callab.sh "RAVA_CALL_PLAN=1" 5 "static-nop"
set -u
ARM="$1"; PAIRS="${2:-5}"; ROW="${3:-static-nop}"
export RAVA_JIT=0 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584
A=(); B=()
# HOTNS is CallCost's aggregate (sum of the four drift-cancelled minima, in ps/call); any
# other ROW name selects that single row. The aggregate is the quieter signal.
grab() {
  local out
  out=$(env $1 ./target/release/rava.exe -cp out CallCost 2>/dev/null | tr -d '')
  if [ "$ROW" = "HOTNS" ]; then
    printf '%s
' "$out" | grep '^HOTNS=' | cut -d= -f2
  else
    printf '%s
' "$out" | grep -- "$ROW" | sed 's/.*CALLPS=\(-\{0,1\}[0-9]*\).*//'
  fi
}
for i in $(seq 1 "$PAIRS"); do
  if [ $((i % 2)) -eq 1 ]; then A+=($(grab "$ARM")); B+=($(grab "RAVA_X_NONE=1"));
  else B+=($(grab "RAVA_X_NONE=1")); A+=($(grab "$ARM")); fi
done
mn() { printf '%s\n' "$@" | sort -n | head -1; }
mx() { printf '%s\n' "$@" | sort -n | tail -1; }
am=$(mn "${A[@]}"); bm=$(mn "${B[@]}"); ax=$(mx "${A[@]}"); bx=$(mx "${B[@]}")
echo "row=$ROW  arm=[$ARM]"
echo "  A(arm)  ${A[*]}   min=$am"
echo "  B(base) ${B[*]}   min=$bm"
if [ "$ax" -lt "$bm" ]; then verdict="every A below every B (DISJOINT, for)";
elif [ "$am" -gt "$bx" ]; then verdict="every A above every B (DISJOINT, against)";
else verdict="arms overlap"; fi
echo "  A vs B: $(awk -v a="$am" -v b="$bm" 'BEGIN{printf "%.2f%%", (a-b)*100/b}')   $verdict"

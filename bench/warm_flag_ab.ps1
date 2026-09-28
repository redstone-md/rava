# WARM, ALTERNATING flag A/B on the BOOT.
#
# WHY THIS EXISTS. `boot_census.ps1` run in blocks reads the FIRST boot of a batch 20-40% faster
# than every boot after it, on either arm — this box boosts when idle and settles once it is warm.
# A batch of "off" runs followed by a batch of "on" runs therefore compares a COLD arm against a
# WARM one and reports a regression that is entirely the thermal state. That mistake was made
# twice in one session and diagnosed as a busy box both times, because re-running after a pause
# let the box cool and "confirmed" the baseline.
#
# So: one warmup boot whose reading is DISCARDED, then strict ABBA alternation.
param(
  [Parameter(Mandatory = $true)][string]$Flags,   # e.g. 'RAVA_ARENA_SYNTH=1;RAVA_ARENA_NATIVE_NEW=1'
  [string]$Base = 'RAVA_LANE_PROBE=1',
  [int]$Pairs = 3,
  [int]$TimeoutSec = 600
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
function Boot([string]$flags, [string]$tag) {
  $env:RAVA_CENSUS_FLAGS = $flags
  $out = & .\bench\boot_census.ps1 -Tag $tag -TimeoutSec $TimeoutSec 2>&1 | Out-String
  $m = [regex]::Match($out, 'wall=([\d.]+)s')
  if ($m.Success) { [double]$m.Groups[1].Value } else { -1 }
}
$w = Boot $Base 'warm_discard'
"warmup (DISCARDED): $w"
$off = @(); $on = @()
foreach ($i in 1..$Pairs) {
  # ABBA: off,on then on,off — so a monotone drift cancels within the pair-of-pairs.
  $order = if ($i % 2 -eq 1) { @('off','on') } else { @('on','off') }
  foreach ($s in $order) {
    $f = if ($s -eq 'on') { "$Base;$Flags" } else { $Base }
    $v = Boot $f "warm_$s$i"
    if ($s -eq 'on') { $on += $v } else { $off += $v }
    "$s`t$v"
  }
}
""
"off: n=$($off.Count) min=$(($off | Measure-Object -Minimum).Minimum) runs=$($off -join '/')"
"on : n=$($on.Count) min=$(($on | Measure-Object -Minimum).Minimum) runs=$($on -join '/')"
$d = @(); for ($i = 0; $i -lt [Math]::Min($off.Count,$on.Count); $i++) { $d += [math]::Round($on[$i] - $off[$i], 2) }
"paired (on minus off): $($d -join '  ')   favouring ON: $(($d | Where-Object { $_ -lt 0 }).Count)/$($d.Count)"

# INTERPRETER A/B — two binaries, alternating, pinned, on a QUIET machine.
#
# Every interpreter step in this program is a few percent, and this box has a documented 2x
# bimodal spread when a run lands on an E-core (ledger row 187) plus whatever else is running.
# Two measurements taken today were corrupted by a background workflow, so the harness now
# REFUSES to run while another rava.exe or cargo build is alive rather than producing a number
# nobody can trust.
#
#   bench\interp_ab.ps1 -Base HEAD~1 -N 9
#
# -Base is any git ref; it is built in a worktree with its own target dir, so neither side
# rebuilds the other. The working tree is the "after" side.
param(
  [string]$Base = 'HEAD~1',
  [int]$N = 9,
  [string]$Class = 'CallBench2',
  [string]$BenchArgs = '4000000',
  [string]$Pattern = 'RUN1_NS',
  [string]$Jit = '0'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$root = (Get-Location).Path

# A busy machine is not a measurement. Names, not counts: "something else is running" is the
# single most common way a percent-level A/B lies.
$busy = @(Get-Process rava, cargo, rustc -ErrorAction SilentlyContinue | Select-Object -Expand Name -Unique)
if ($busy) {
  "REFUSING: {0} still running — a percent-level A/B needs a quiet box." -f ($busy -join ', ')
  exit 1
}

$wt = Join-Path $env:TEMP 'rava-interp-ab'
if (Test-Path $wt) { git worktree remove $wt --force 2>&1 | Out-Null }
git worktree add $wt $Base 2>&1 | Select-Object -Last 1
Push-Location $wt
cargo build -p rava-cli --features next-gen-gc --release 2>&1 | Select-Object -Last 1
Pop-Location
cargo build -p rava-cli --features next-gen-gc --release 2>&1 | Select-Object -Last 1

$exes = [ordered]@{
  before = Join-Path $wt 'target\release\rava.exe'
  after  = Join-Path $root 'target\release\rava.exe'
}
$env:RAVA_JIT = $Jit
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
$res = @{ before = @(); after = @() }
$argv = @('-cp', 'out', $Class) + $BenchArgs.Split(' ')

for ($i = 1; $i -le $N; $i++) {
  foreach ($side in @('before', 'after')) {
    $o = Join-Path $root "bench\interp_ab_$side.txt"
    $p = Start-Process -FilePath $exes[$side] -ArgumentList $argv -WorkingDirectory $root `
          -RedirectStandardOutput $o -RedirectStandardError "$o.err" -PassThru -WindowStyle Hidden
    $p.ProcessorAffinity = [IntPtr]0x0FFF   # P-cores only
    $p.PriorityClass = 'High'
    if (-not $p.WaitForExit(300000)) { $p.Kill(); $p.WaitForExit(); continue }
    $m = Select-String -Path $o -Pattern "$Pattern=(.*)"
    if ($m) { $res[$side] += [double]$m.Matches[0].Groups[1].Value }
  }
}

foreach ($side in @('before', 'after')) {
  $v = @($res[$side] | Sort-Object)
  if ($v.Count -eq 0) { "{0,-7} no samples" -f $side; continue }
  "{0,-7} min={1,7:N1} med={2,7:N1} mean={3,7:N1} max={4,7:N1} n={5}" -f `
    $side, $v[0], $v[[int]([math]::Floor($v.Count / 2))], ($v | Measure-Object -Average).Average, $v[-1], $v.Count
}
# TWO verdicts, because they answer different questions and this harness gave the wrong one first.
#
# Disjoint ranges are the strongest claim, but they are the wrong test for an ALTERNATING A/B: the
# runs are PAIRED (before_i and after_i sit microseconds apart, under the same thermal and
# scheduling weather), and a single outlier on either side destroys a range comparison while
# leaving every pair intact. So report the paired statistic too: how many pairs the change won,
# and the median of the per-pair deltas. A drift that moves both sides equally cancels in a pair
# and does not in a range.
$b = @($res.before); $a = @($res.after)
if ($b.Count -and $a.Count) {
  $bs = @($b | Sort-Object); $as = @($a | Sort-Object)
  $disjoint = ($as[-1] -lt $bs[0]) -or ($bs[-1] -lt $as[0])
  $pairs = [Math]::Min($b.Count, $a.Count)
  $deltas = 0..($pairs - 1) | ForEach-Object { (($a[$_] / $b[$_]) - 1) * 100 }
  $wins = ($deltas | Where-Object { $_ -lt 0 }).Count
  $ds = @($deltas | Sort-Object)
  "delta med={0,6:N1}%  ranges {1}" -f `
    ((($as[[int]([math]::Floor($as.Count / 2))] / $bs[[int]([math]::Floor($bs.Count / 2))]) - 1) * 100), `
    $(if ($disjoint) { 'DISJOINT' } else { 'overlapping' })
  "paired: after faster in {0}/{1} pairs, median per-pair delta {2,6:N1}%" -f `
    $wins, $pairs, $ds[[int]([math]::Floor($ds.Count / 2))]
}
git worktree remove $wt --force 2>&1 | Out-Null

# PAIRED A/B on a GATE — fixed work, alternating binaries, hash-verified.
#
# WHY THIS AND NOT THE BOOT. A boot cannot measure a 1-2% change by any statistic
# available on it: its wall spreads ±4% from other processes and its CPU spreads
# 7-15% because the JIT compiler threads and the ring's workers do a different
# AMOUNT of work run to run (compilation order, task stealing). A gate does the
# SAME work every time — that is what its hash proves — so its run-to-run spread
# is machine noise alone, and paired alternation cancels the drift that is left.
#
# THE HASH IS CHECKED ON EVERY RUN, not once. A speed A/B whose arms compute
# different answers is not a speed A/B, and this is the harness most likely to be
# pointed at a change that alters behaviour by accident.
#
# Reports mean and MIN. Min is the run that met the least interference and is the
# better estimator of "how long does this work take"; the ledger's rule that a
# gate's HASH is the instrument and its MILLISECONDS are not applies to a SINGLE
# reading — a paired, alternating, hash-verified series is a different claim.
#
#   bench\gate_ab.ps1 -BeforeExe bench\bin\a.exe -AfterExe bench\bin\b.exe -Gate ringscan -N 7
param(
  [Parameter(Mandatory = $true)][string]$BeforeExe,
  [Parameter(Mandatory = $true)][string]$AfterExe,
  [ValidateSet('chunkfill', 'ringscan', 'ringlane', 'ringlanei')][string]$Gate = 'ringscan',
  [int]$N = 7,
  # How many times to wait out a busy box before giving up.
  [int]$Tries = 10,
  # Set NAME=VALUE on the AFTER arm only. With the same binary on both sides this
  # attributes a default-off (or default-on, via its opt-out) feature on fixed
  # work — which is what a stacked win has to be decomposed with, since a stack
  # that measures -20% says nothing about which of its members earned it.
  [string]$Flag = ''
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
. "$PSScriptRoot\require_quiet.ps1"
. "$PSScriptRoot\assert_flag.ps1"
# The flag is applied to the AFTER arm only, so that is the binary that has to know it.
# Without this a stale build reports a flawless, perfectly-paired NOTHING.
if ($Flag) { Assert-FlagKnown -Exe $AfterExe -Flag $Flag }
# WAIT for a quiet box rather than refusing once. A refusal is not a result, and a
# busy box is a temporary condition — every launcher of this harness would
# otherwise have to reimplement the loop, and one of them did not and lost a run.
Wait-QuietBox -Context 'the gate A/B' -Tries $Tries
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"
javac -nowarn -cp $mccp -d out bench\ChunkFill.java bench\RingScan.java bench\RingLane.java bench\RingLaneInstance.java
# `timer` is the gate's OWN internal measurement of the fixed work, which is the
# quantity this harness exists to compare. Process wall additionally carries JVM
# startup, class loading and teardown — on ChunkFill that is ~31 s of a ~37 s
# run, i.e. six times the work being measured, and all of its noise lands in the
# comparison. The gate already prints the number without any of that.
$spec = @{
  chunkfill = @{ args = @('ChunkFill', '1', '64'); want = '8790f04ace726ecf'; timer = 'FILL_MS' }
  ringscan  = @{ args = @('RingScan');             want = '37539c6c375d0c3'; timer = 'RINGS_MS' }
  ringlane  = @{ args = @('RingLane');             want = '37539c6c375d0c3'; timer = 'LANE_MS' }
  ringlanei = @{ args = @('RingLaneInstance');     want = '37539c6c375d0c3'; timer = 'LANE_MS' }
}[$Gate]
$env:RAVA_JIT = '1'
$exes = @{ before = $BeforeExe; after = $AfterExe }
foreach ($s in 'before', 'after') { if (-not (Test-Path $exes[$s])) { "missing $s : $($exes[$s])"; exit 3 } }
"gate=$Gate  before=$BeforeExe  after=$AfterExe{0}" -f $(if ($Flag) { "  flag(after-only)=$Flag" } else { '' })
$res = @{ before = @(); after = @() }
$bad = $false
foreach ($i in 1..$N) {
  # ABBA, matching wt_boot_ab.ps1 — but for a different reason, and the difference is
  # the result. The BOOT harness was changed to ABBA because a self-A/B caught it
  # reading +3.56% of `Done` for a binary against a byte-identical copy of itself.
  # THIS harness was put through the same self-A/B and has no such fault to fix:
  # against a byte-identical copy it read -0.06% (3/6 pairs) under the old ABAB and
  # +0.37% (2/6) under this ABBA, two orders differing by less than ONE pair's noise.
  # So the swap buys symmetry by construction, not a measured correction, and the tree
  # gets one alternation convention instead of two.
  #
  # What those twelve known-zero pairs DO establish is the resolution: pooled paired
  # mean +0.18%, per-pair range -2.13% to +2.70%. FILL_MS resolves ~±0.4% at n=6 and
  # ~±2.7% at n=1 — so a two-percent verdict from one or two pairs is noise, however
  # cleanly it prints.
  $order = if ($i % 2 -eq 1) { @('before', 'after') } else { @('after', 'before') }
  foreach ($side in $order) {
    if ($Flag) {
      $kv = $Flag.Split('=', 2)
      if ($side -eq 'after') { Set-Item "env:$($kv[0])" $kv[1] } else { Remove-Item "env:$($kv[0])" -ErrorAction SilentlyContinue }
    }
    robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
    $out = "bench\gab_$side.log"
    Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $exes[$side] -ArgumentList (@('-cp', $cp) + $spec.args) `
          -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
          -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
    try { $p.ProcessorAffinity = [IntPtr]0x0FFF; $p.PriorityClass = 'High' } catch {}
    if (-not $p.WaitForExit(600000)) { $p.Kill(); "$side run ${i}: TIMEOUT"; $bad = $true; continue }
    $sw.Stop()
    $cpu = 0.0
    try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
    $txt = Get-Content $out
    $hm = ($txt | Select-String 'HASH=([0-9a-f]+)' | Select-Object -First 1)
    $h = if ($hm) { $hm.Matches[0].Groups[1].Value } else { '<none>' }
    if ($h -ne $spec.want) { "*** $side run ${i}: HASH $h != $($spec.want) — the arms do not compute the same answer"; $bad = $true }
    $tm = ($txt | Select-String "$($spec.timer)=(\d+)" | Select-Object -First 1)
    if (-not $tm) { "*** $side run ${i}: no $($spec.timer) in the gate output"; $bad = $true; continue }
    $work = [double]$tm.Matches[0].Groups[1].Value
    $res[$side] += [PSCustomObject]@{ ms = $work; wall = $sw.Elapsed.TotalMilliseconds; cpu = $cpu }
    "{0,-7} run {1}: {2}={3,8:N0} ms  (wall {4,8:N0} ms)  hash={5}" -f $side, $i, $spec.timer, $work, $sw.Elapsed.TotalMilliseconds, $h
  }
}
if ($bad) { '*** VERDICT WITHHELD: a run timed out or an arm computed a different hash.'; exit 2 }
$stat = @{}
foreach ($side in 'before', 'after') {
  $stat[$side] = @{
    mean = ($res[$side] | Measure-Object ms -Average).Average
    min  = ($res[$side] | Measure-Object ms -Minimum).Minimum
    cpu  = ($res[$side] | Measure-Object cpu -Minimum).Minimum
  }
  "{0,-7} {1}: mean={2,8:N0} ms  min={3,8:N0} ms   (min wall {4,8:N0} ms)  n={5}" -f `
    $side, $spec.timer, $stat[$side].mean, $stat[$side].min, ($res[$side] | Measure-Object wall -Minimum).Minimum, $res[$side].Count
}
# WIN RATE over the PAIRS, which is what alternation buys: a per-pair comparison
# is immune to drift across the batch, and 7 of 7 is a claim a mean cannot make.
$wins = 0
for ($i = 0; $i -lt $res['before'].Count; $i++) { if ($res['after'][$i].ms -lt $res['before'][$i].ms) { $wins++ } }
$dmean = 100 * ($stat['after'].mean - $stat['before'].mean) / $stat['before'].mean
$dmin = 100 * ($stat['after'].min - $stat['before'].min) / $stat['before'].min
"delta mean={0,6:N2}%  min={1,6:N2}%  after-wins {2}/{3} pairs" -f $dmean, $dmin, $wins, $res['before'].Count
if ($Flag) { Remove-Item "env:$($Flag.Split('=',2)[0])" -ErrorAction SilentlyContinue }
exit 0

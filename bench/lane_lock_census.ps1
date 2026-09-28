# WHAT the lane's workers actually park on. `RAVA_LANE_PROBE=<ms>` dumps per-worker
# locks / wait_ms / hold_ms plus the locked-callee composition tally, which is the
# only instrument that can name the symbols behind the general-path resolve lock —
# the mechanism ring_concurrency.ps1 identified (same threads, same work, 1.8x the
# task time, LESS CPU = parked workers).
#
# Two arms: the default threshold, and a lowered one that reproduces the stall in
# 3 of 4 runs. The DIFFERENCE between their tallies is the fix list.
# Diagnostic only — these walls are not timing-valid.
param([string]$Exe = 'target\pgo-fat\rava.exe', [int]$ProbeMs = 3000, [int]$TimeoutSec = 300)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

foreach ($arm in @(@{ tag = 'th512'; th = '' }, @{ tag = 'th64'; th = '64' })) {
  Remove-Item Env:RAVA_JIT_THRESHOLD -ErrorAction SilentlyContinue
  $env:RAVA_JIT = '1'
  $env:RAVA_LANE_PROBE = "$ProbeMs"
  if ($arm.th) { $env:RAVA_JIT_THRESHOLD = $arm.th }

  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\lanelock_$($arm.tag).log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
  $dl = [DateTime]::Now.AddSeconds($TimeoutSec)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 500
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  Write-Host ("== {0}  {1}" -f $arm.tag, (Get-Content $out -Tail 1))
  Get-Content "$out.err" | Select-String 'LANE-PROBE' | Select-Object -Last 2 | ForEach-Object { $_.Line }
  Get-Content "$out.err" | Select-String 'LOCKED-CALLEE|locked callee|hold_ms=' | Select-Object -Last 14 | ForEach-Object { $_.Line }
}
Remove-Item Env:RAVA_LANE_PROBE, Env:RAVA_JIT_THRESHOLD -ErrorAction SilentlyContinue

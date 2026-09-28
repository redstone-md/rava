# WHICH callback kind grows under RAVA_ARENA_LAMBDA?
#
# After the comparator-plan fix the flag's remaining cost is +3 s in block A+B, and the
# wall category split says JIT_CALLBACK 684 -> 822 ticks (+20%) while native bridges and
# interp both FALL. The callback census is split into 28 named kinds, so this names the
# growing one.
#
# ⚠ A COUNT is not a COST. This tree has refuted the count-to-price model twice (removing
# 12.6% of all PIC misses was worth zero). Use this only to NAME a suspect, then price it
# with the RIP sampler.
param([string]$Exe = 'target\release-diag\rava.exe', [int]$TimeoutSec = 300)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

foreach ($arm in @(@('off', $null), @('on', '1'))) {
  Remove-Item Env:RAVA_ARENA_LAMBDA -ErrorAction SilentlyContinue
  $env:RAVA_JIT = '1'
  $env:RAVA_CALLBACK_STATS = '1'
  $env:RAVA_HEARTBEAT = '1'
  $env:RAVA_JIT_DIRECT_PROBE = '1'
  if ($arm[1]) { $env:RAVA_ARENA_LAMBDA = $arm[1] }
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\cbsplit_$($arm[0]).log"
  Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
  $dl = [DateTime]::Now.AddSeconds($TimeoutSec)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 500
    if ($p.HasExited) { break }
    if ((Get-Content $out -Tail 2 -ErrorAction SilentlyContinue) -match 'Done \(') { break }
  }
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  Write-Host ("== {0}: {1}" -f $arm[0], (Get-Content $out -Tail 1))
  Get-Content "$out.err" | Select-String 'CALLBACKS|DIRECT-MISS|\[CB\]' | Select-Object -Last 6 |
    ForEach-Object { "   " + $_.Line }
}
Remove-Item Env:RAVA_HEARTBEAT, Env:RAVA_JIT_DIRECT_PROBE, Env:RAVA_CALLBACK_STATS, Env:RAVA_ARENA_LAMBDA -ErrorAction SilentlyContinue

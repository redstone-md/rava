# WHERE does the ring's time actually go? RIP sampling on RingScan alone.
#
# The ring is ~80 of the boot's ~115 CPU-seconds and runs 11.4x slower than the JVM on
# a bit-exact workload (bench/jvm_ring_calibrate.ps1), so this ratio decides which
# lever class can possibly pay: samples INSIDE compiled machine code are a codegen
# problem, samples in the Rust host are a dispatch/allocator/lookup problem.
#
# ⚠ release-diag ONLY — `release` is stripped and MIS-NAMES symbols, which has produced
# wrong findings in this tree before. The diag WALL is not a number to quote; this
# measures a ratio.
param(
  [string]$Exe = 'target\release-diag\rava.exe',
  [string]$Main = 'RingScan',
  [int]$TimeoutSec = 900
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
if (-not (Test-Path $Exe)) {
  throw "missing $Exe — cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile"
}
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
foreach ($k in 'RAVA_JIT_STATS', 'RAVA_LANE_PROBE', 'RAVA_JIT_DIRECT_PROBE', 'RAVA_FREEZE_CENSUS') {
  Remove-Item "env:$k" -ErrorAction SilentlyContinue
}
$env:RAVA_JIT = '1'
$env:RAVA_PROFILE_WALL = '1'
$env:RAVA_PROFILE_PC = '1'
$out = "bench\ringrip_$Main.log"
Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, $Main) `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
if (-not $p.WaitForExit($TimeoutSec * 1000)) { $p.Kill(); throw 'timeout' }
Remove-Item Env:RAVA_PROFILE_WALL, Env:RAVA_PROFILE_PC -ErrorAction SilentlyContinue
Write-Host (Get-Content $out -Tail 1)
Get-Content "$out.err" | Select-String 'PC-PROFILE|RIP samples|WALL-CAT|inside compiled' |
  Select-Object -First 40 | ForEach-Object { $_.Line }

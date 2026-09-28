# Reproducible boot-bench harness — kills Windows scheduler noise (E-core hopping + freq scaling)
# that otherwise inflates ChunkFill SETUP_MS by up to 2.1x on hybrid Intel (i5-14400F: 6P+4E).
#   .\bench_run.ps1 -Cores ecore -Runs 5     # E-cores (0xF000), keeps P-cores free — regression A/B
#   .\bench_run.ps1 -Cores pcore -Runs 3     # P-cores  (0x0FFF)               — honest vs-JVM absolute
# Both force High-Performance plan (fixes freq scaling) and restore the original plan after.
param(
  [ValidateSet('ecore','pcore')] [string]$Cores = 'ecore',
  [int]$Runs = 5,
  [string]$Main = 'ChunkFill'
)
$ErrorActionPreference = 'Continue'
$mask = if ($Cores -eq 'ecore') { 0xF000 } else { 0x0FFF }   # 14400F: logical 12-15 = E, 0-11 = P
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

# fix frequency: force High Performance, restore original after
$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null

$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_JIT_TIER0='1'

Write-Host "== bench: cores=$Cores mask=0x$('{0:X}' -f $mask) runs=$Runs main=$Main plan=High-Perf =="
$setups = @()
foreach ($i in 1..$Runs) {
  Get-Process rava -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
  $log = Join-Path $here "bench_$($Cores)_$i.log"
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath (Join-Path $here '..\target\release\rava.exe') `
        -ArgumentList @('-cp', $cp, $Main) -WorkingDirectory $here `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity = [IntPtr]$mask
  $p.WaitForExit(600000) | Out-Null
  $sw.Stop()
  $s  = (Select-String $log -Pattern 'SETUP_MS=(\d+)').Matches.Groups[1].Value | Select-Object -First 1
  $h  = (Select-String $log -Pattern 'ChunkFill_HASH=([0-9a-f]+)').Matches.Groups[1].Value | Select-Object -First 1
  if ($s) { $setups += [int]$s }
  Write-Host ("run{0}: SETUP_MS={1} wall={2:N1}s hash={3}" -f $i,$s,$sw.Elapsed.TotalSeconds,$h)
}
powercfg /setactive $orig | Out-Null
if ($setups.Count) {
  $sorted = $setups | Sort-Object
  $med = $sorted[[int]($sorted.Count/2)]
  $spread = if ($sorted[0]) { [math]::Round(100.0*($sorted[-1]-$sorted[0])/$sorted[0],1) } else { 0 }
  Write-Host ("== median SETUP_MS={0}  spread={1}%  (plan restored) ==" -f $med,$spread)
}

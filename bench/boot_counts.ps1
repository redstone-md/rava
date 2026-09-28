# Boot once with a diagnostic env var armed and keep the stderr, WITHOUT the
# quiet check.
#
# The quiet check exists to protect TIMINGS. A COUNT — how many methods were
# compiled, how many callbacks fired, how many sites missed — does not move
# because a browser is open, so refusing to collect one on a busy box just means
# not collecting it. Same world restore, same flags and same port as
# `prod_boot.ps1`, so the run is the same workload; only the guard is gone, and
# the wall it prints is explicitly NOT a measurement.
#
#   bench\boot_counts.ps1 -Env RAVA_JIT_STATS=1 -Tag jitstats
param(
  [string[]]$Env = @(),
  [string]$Tag = 'counts',
  [string]$Exe = 'target\pgo-fat\rava.exe',
  [int]$TimeoutS = 300
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
if (-not (Test-Path $Exe)) { "missing $Exe"; exit 3 }
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'; $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$set = @()
foreach ($e in $Env) {
  $kv = $e.Split('=', 2)
  Set-Item "env:$($kv[0])" $kv[1]
  $set += $kv[0]
}
$log = "bench\$Tag.log"
Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
      -PassThru -WindowStyle Hidden
$hit = $false
while ($sw.Elapsed.TotalSeconds -lt $TimeoutS) {
  Start-Sleep -Milliseconds 250
  if ($p.HasExited) { break }
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { $hit = $true; break }
}
Start-Sleep -Milliseconds 1200   # let any at-exit dump land
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
foreach ($k in $set) { Remove-Item "env:$k" -ErrorAction SilentlyContinue }
"{0}: reachedDone={1} wall={2}s (NOT a measurement) -> {3}.err" -f `
  $Tag, $hit, [math]::Round($sw.Elapsed.TotalSeconds, 1), $log

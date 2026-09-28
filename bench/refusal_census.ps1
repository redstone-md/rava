# WHY DOES COMPILED CODE REFUSE A DIRECT CALL? One boot, the refusal histogram, and the split of
# the one site that carries most of it.
#
# `dresolve::report()` and `clause_report()` are printed by the lane probe, so BOTH flags are
# needed: RAVA_CALLBACK_STATS arms the counters, RAVA_LANE_PROBE makes something print them.
# Arming only the first produces a table nobody reads — the exact shape of instrument defect this
# tree keeps recording.
param([int]$ProbeMs = 5000, [string]$Exe = 'target\release\rava.exe', [int]$TimeoutS = 300)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'; $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$env:RAVA_CALLBACK_STATS = '1'
$env:RAVA_LANE_PROBE = "$ProbeMs"
$log = 'bench\refusal_census.log'
Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
      -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds($TimeoutS)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 300
  if ($p.HasExited) { break }
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) {
    Start-Sleep -Seconds ([math]::Ceiling($ProbeMs / 1000) + 1)   # let one more probe line land
    break
  }
}
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
foreach ($k in 'RAVA_CALLBACK_STATS', 'RAVA_LANE_PROBE') { Remove-Item "env:$k" -ErrorAction SilentlyContinue }
"{0}s -> {1}.err" -f [math]::Round($sw.Elapsed.TotalSeconds, 1), $log
Select-String -Path $log -Pattern 'Done \(' | Select-Object -Last 1

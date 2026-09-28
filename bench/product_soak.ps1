# DOES THE SERVER STAY UP AFTER `Done`? — the question a 10× boot does NOT answer.
#
# ⭐`bench/offload_repro.ps1` stops ~1 s after `Done`, which is exactly the wrong moment to stop
# when the whole point of the offload is that work MOVED past `Done` instead of blocking it. The
# stronghold search MC hands to `Util.backgroundExecutor()` is still running when the boot
# finishes — on the JVM too — so "did it boot fast" and "is it a working server" are two
# measurements, and only the second one licenses making the flag default.
#
# PRODUCT CONFIGURATION (compilers ON). `offload_soak.ps1` runs the STANDING benchmark
# (`RAVA_JIT=0`), where the deferral lane is not armed at all — `rings()` requires the JIT — so it
# cannot answer anything about the lane. This one arms it, which is the only way to soak a change
# to the drain's scope.
#
# Boots, waits for `Done`, then holds for `-SoakS` seconds and reports: still alive, exit code,
# throws that appeared AFTER `Done`, and peak working set.
param(
  [int]$SoakS = 300,
  [int]$BootTimeoutS = 2400,
  [switch]$NoFlag,
  [string]$Tag = 'soak'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
& robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'; $env:RAVA_TRACE_THROW = '1'
if ($NoFlag) { Remove-Item env:RAVA_ASYNC_ANY_EXECUTOR -EA SilentlyContinue }
else { $env:RAVA_ASYNC_ANY_EXECUTOR = '1' }
$log = "bench\$Tag.log"; $err = "bench\$Tag.err"
Remove-Item $log, $err -EA SilentlyContinue
$p = Start-Process -FilePath (Join-Path (Get-Location) 'target\release\rava.exe') `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError $err `
      -PassThru -WindowStyle Hidden
$null = $p.Handle
try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
$dl = [DateTime]::Now.AddSeconds($BootTimeoutS)
while ([DateTime]::Now -lt $dl -and -not $p.HasExited) {
  Start-Sleep -Seconds 2
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { break }
}
$m = Select-String -Path $log -Pattern 'Done \(([\d.]+)s\)' | Select-Object -Last 1
if (-not $m) {
  Write-Host ("NEVER REACHED Done (exited={0}); soak not attempted" -f $p.HasExited)
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  exit 1
}
Write-Host ("Done={0}  — soaking {1}s" -f $m.Matches.Groups[1].Value, $SoakS)
# Everything already on stderr belongs to the BOOT; only what arrives after this line is a soak
# finding. Counting instead of diffing keeps it robust to the log being appended concurrently.
$throwsAtDone = (Select-String -Path $err -Pattern '^\[THROW\]' -EA SilentlyContinue).Count
$logAtDone = (Get-Content $log).Count
$soakEnd = [DateTime]::Now.AddSeconds($SoakS)
$peakMB = 0
while ([DateTime]::Now -lt $soakEnd -and -not $p.HasExited) {
  Start-Sleep -Seconds 5
  try { $p.Refresh(); $mb = [int]($p.PeakWorkingSet64 / 1MB); if ($mb -gt $peakMB) { $peakMB = $mb } } catch {}
}
$alive = -not $p.HasExited
$code = if ($alive) { 'still running' } else { try { $p.ExitCode } catch { 'n/a' } }
$throwsAfter = (Select-String -Path $err -Pattern '^\[THROW\]' -EA SilentlyContinue).Count - $throwsAtDone
Write-Host ("SOAK {0}s: alive={1} exit={2} newThrows={3} peakRSS={4}MB" -f $SoakS, $alive, $code, $throwsAfter, $peakMB)
if ($throwsAfter -gt 0) {
  Write-Host '--- throws after Done ---'
  Select-String -Path $err -Pattern '^\[THROW\]' | Select-Object -Last $throwsAfter | ForEach-Object { $_.Line }
}
Write-Host '--- log after Done ---'
Get-Content $log | Select-Object -Skip $logAtDone | Select-Object -First 12
if ($alive) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }

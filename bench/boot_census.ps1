# Real-boot COVERAGE CENSUS. Not a timing run — it reports COUNTS, which are immune to the
# core contention that makes wall figures on this box unusable while other work is in flight.
# Answers the value-model arc's only open question on the workload of record: what fraction of
# the REAL boot's live reference cells are already untagged 0b00, and which classes own the rest.
param([string]$Exe = 'target\release\rava.exe', [string]$Tag = 'census', [int]$TimeoutSec = 900)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_JIT_TIER0='1'
if ($env:RAVA_CENSUS_FLAGS) { foreach ($kv in $env:RAVA_CENSUS_FLAGS.Split(";")) { $p2=$kv.Split("=",2); Set-Item "env:$($p2[0])" $p2[1] } } else { $env:RAVA_CELL_CENSUS="1" }

robocopy (Join-Path $root 'world_pristine') (Join-Path $root 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$log = Join-Path $here "census_$Tag.log"
Remove-Item $log,"$log.err" -ErrorAction SilentlyContinue
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath (Join-Path $root $Exe) `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$done = $false
while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
  if ($p.HasExited) { break }
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { $done = $true; break }
  if ((Test-Path "$log.err") -and (Select-String -Path "$log.err" -Pattern 'Done \(' -Quiet)) { $done = $true; break }
  Start-Sleep -Milliseconds 500
}
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
Write-Host ("census run: done={0} wall={1:N1}s (wall is NOT a measurement here)" -f $done,$sw.Elapsed.TotalSeconds)
$bind = (Test-Path "$log.err") -and (Select-String -Path "$log.err" -Pattern 'FAILED TO BIND' -Quiet)
if ($bind) { Write-Host "PORT BUSY — another boot held 25599; census invalid" }
Write-Host '--- last CELL-CENSUS ---'
Select-String -Path "$log.err" -Pattern '\[CELL-CENSUS\]' | Select-Object -Last 12 | ForEach-Object { $_.Line }
Write-Host '--- last HDR-CENSUS ---'
Select-String -Path "$log.err" -Pattern '\[HDR-CENSUS\]' | Select-Object -Last 5 | ForEach-Object { $_.Line }

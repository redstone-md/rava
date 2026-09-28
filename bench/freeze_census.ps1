# WHY the lock is still taken 82.5 M times per ring even though the freeze-resolve
# table is default-ON. RAVA_FREEZE_CENSUS classifies every compiled dispatch MISS by
# which PRE-LOCK arm would have served it — the freeze-resolve table, the frozen
# direct cache, or nothing (⇒ the one global resolve lock). Read-only, measured on
# the ordinary serial boot, so it reports the COVERAGE CEILING of the lock-free arms.
param([string]$Exe = 'target\pgo-fat\rava.exe', [int]$TimeoutSec = 300)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_FREEZE_CENSUS = '1'
$env:RAVA_M3_FREEZE_RESOLVE = '1'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\freeze_census.log'
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
Remove-Item Env:RAVA_FREEZE_CENSUS, Env:RAVA_M3_FREEZE_RESOLVE -ErrorAction SilentlyContinue
Write-Host (Get-Content $out -Tail 1)
Get-Content "$out.err" | Select-String 'FREEZE-CENSUS|FREEZE|census' | Select-Object -Last 20 | ForEach-Object { $_.Line }

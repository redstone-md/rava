# WHY does the lane admit tasks in the SERIAL prefix and then claim none of them?
#
# The wall arithmetic says the goal is reachable by parallelism alone: 95.2 CPU-s over
# 12 threads is an 8 s floor, and today ~30 s of the 47.6 s wall is the serial prefix
# running on ONE core. The lane already tries to fork part of it - an early batch shows
# `admit#0 ReloadableServerRegistries::lambda$scheduleRegistryLoad$3` followed by
# `claimed=0/2 ... nursery_full=2` - so the tasks pass ADMISSION and fail at CLAIM.
#
# This prints every batch ledger line of a boot, so the claim failures are counted and
# attributed instead of inferred from one remembered line.
param([string]$Exe = 'target\pgo-fat\rava.exe', [int]$TimeoutSec = 200)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\claims.log'
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
Write-Host (Get-Content $out -Tail 1)
Write-Host "--- every batch ledger line:"
Get-Content "$out.err" | Select-String 'batch n=' | ForEach-Object {
  ($_.Line -replace '^\[GILFREE-RINGS\] ', '') -replace ' (cbs_min|pilot_cbs|slice|pool|pool_used|pool_claims)=\S+', ''
}
Write-Host "--- admitted impls:"
Get-Content "$out.err" | Select-String 'admit#' | ForEach-Object { ($_.Line -split '::')[1] } |
  Group-Object | Sort-Object Count -Descending | Select-Object -First 8 |
  ForEach-Object { "{0,6}x {1}" -f $_.Count, $_.Name }

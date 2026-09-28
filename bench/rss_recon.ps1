# RSS DECOMPOSITION on a product boot. Prints the [CENSUS] rss-recon line, which splits the
# resident set into malloc_live / arena committed / unaccounted (mimalloc overhead + stacks +
# image + JIT pages). The point: rava boots MC in 774 MB where `java -Xint` needs 351, every
# slicing axis of the profile is flat, and a uniform 2.2x resident set is the one candidate
# that would tax EVERY axis equally. Before curing it, find out what it IS.
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
& robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$env:RAVA_JIT='1'; $env:RAVA_JIT_TIER0='1'; $env:RAVA_OFFLOAD_FIRST='1'
$env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'
$env:RAVA_CENSUS='1'
$log='bench\rssrecon.log'
Remove-Item $log,"$log.err" -EA SilentlyContinue
$p = Start-Process -FilePath (Join-Path (Get-Location) 'target\release\rava.exe') `
     -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
     -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
     -PassThru -WindowStyle Hidden
$null=$p.Handle
$peak=0
while (-not $p.HasExited) {
  Start-Sleep -Milliseconds 300
  try { $p.Refresh(); if ($p.WorkingSet64 -gt $peak) { $peak=$p.WorkingSet64 } } catch { break }
  if ((Select-String -Path $log -Pattern 'Done \(' -Quiet -EA SilentlyContinue)) { Start-Sleep 2; break }
  if ($p.TotalProcessorTime.TotalSeconds -gt 400) { break }
}
try { $p.Refresh(); $final=$p.WorkingSet64 } catch { $final=0 }
Write-Host ("peak_ws={0}MB final_ws={1}MB" -f [int]($peak/1MB), [int]($final/1MB))
try { Stop-Process -Id $p.Id -Force -EA SilentlyContinue } catch {}
Select-String -Path "$log.err" -Pattern 'rss-recon' | Select-Object -Last 3 | ForEach-Object { $_.Line }
Select-String -Path $log -Pattern 'Done \(' | Select-Object -Last 1 | ForEach-Object { $_.Line }

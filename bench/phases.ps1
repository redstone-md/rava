# BOOT PHASE MAP — plain product boot, no census, timestamps straight out of MC's own log.
# Which phase to attack next is decided here, not by guessing from a flat profile.
$ErrorActionPreference='Continue'
Set-Location $PSScriptRoot\..
$cp='out;'+((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
& robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$env:RAVA_JIT='1'; $env:RAVA_JIT_TIER0='1'; $env:RAVA_OFFLOAD_FIRST='1'
$env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'
foreach($k in 'RAVA_CENSUS','RAVA_CELL_CENSUS','RAVA_KEYTIME','RAVA_STRUCTKEY_SCAN'){ Remove-Item "env:$k" -EA SilentlyContinue }
$log='bench\phases.log'
Remove-Item $log,"$log.err" -EA SilentlyContinue
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath (Join-Path (Get-Location) 'target\release\rava.exe') `
   -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
   -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
   -PassThru -WindowStyle Hidden
$null=$p.Handle
try { $p.ProcessorAffinity=[IntPtr]0x0FFF } catch {}
while(-not $p.HasExited){
  Start-Sleep -Milliseconds 200
  if(Select-String -Path $log -Pattern 'Done \(' -Quiet -EA SilentlyContinue){ Start-Sleep 1; break }
  if($sw.Elapsed.TotalSeconds -gt 400){ break }
}
$wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
$cpu=0; try { $cpu=[math]::Round($p.TotalProcessorTime.TotalSeconds,1) } catch {}
try { Stop-Process -Id $p.Id -Force -EA SilentlyContinue } catch {}
Write-Host ("wall={0}s cpu={1}s" -f $wall,$cpu)

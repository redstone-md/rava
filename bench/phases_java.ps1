# The SAME phase map on HotSpot, so each phase has a ratio rather than an absolute.
# Port 25599 as always — never the user's 25565.
$ErrorActionPreference='Continue'
Set-Location $PSScriptRoot\..
$cp='out;'+((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
& robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$log='bench\phases_java.log'
Remove-Item $log,"$log.err" -EA SilentlyContinue
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath 'java' -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
   -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
   -PassThru -WindowStyle Hidden
$null=$p.Handle
try { $p.ProcessorAffinity=[IntPtr]0x0FFF } catch {}
while(-not $p.HasExited){
  Start-Sleep -Milliseconds 200
  if(Select-String -Path $log -Pattern 'Done \(' -Quiet -EA SilentlyContinue){ Start-Sleep 1; break }
  if($sw.Elapsed.TotalSeconds -gt 300){ break }
}
$wall=[math]::Round($sw.Elapsed.TotalSeconds,1); $cpu=0
try { $cpu=[math]::Round($p.TotalProcessorTime.TotalSeconds,1) } catch {}
try { Stop-Process -Id $p.Id -Force -EA SilentlyContinue } catch {}
Write-Host ("java wall={0}s cpu={1}s" -f $wall,$cpu)

# Worktree-local twin of boot_flag_ab.ps1: interleaved full-MC-boot A/B for ONE env
# flag, run against THIS worktree's rava.exe. Identical recipe (E-cores by default so
# the DELTA is clean and the desktop keeps the P-cores), but with a longer cap — an
# arm that routes many calls through the host can take far longer than the 700 s the
# shared script allows, and "did not finish" is a worse datum than a real number.
# Port 25599 admits ONE boot at a time, so wait for any foreign rava.exe to clear
# first and never kill a process this script did not start.
param(
  [Parameter(Mandatory=$true)][string]$Flag,
  [string]$Seq = 'off,on,on,off',
  [string]$Extra = '',
  [ValidateSet('ecore','pcore')][string]$Cores = 'ecore',
  [int]$CapS = 1500
)
$mask = if($Cores -eq 'ecore'){ 0xF000 } else { 0x0FFF }
$root='D:\code\rava'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe  = Join-Path $here '..\target\release\rava.exe'
$cp='out;'+((Get-Content "$root\bench\mc-cp.txt" -Raw).Trim())
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
if($Extra){ foreach($kv in $Extra.Split(';')){ if($kv){ $p=$kv.Split('='); Set-Item "env:$($p[0])" $p[1] } } }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null
$csv="run,cfg,done_s,reached,wall_s`n"; $i=0
foreach($cfg in ($Seq -split ',')){
  $i++
  # Only a BOOT holds 25599; a bench (ChunkFill/RingScan) binds nothing, so waiting on
  # "any rava.exe" would stall behind work that cannot collide. Wait on the port.
  $waited = 0
  while ((Get-NetTCPConnection -LocalPort 25599 -EA SilentlyContinue) -and $waited -lt 1800) {
    Start-Sleep -Seconds 15; $waited += 15
  }
  if($cfg -eq 'on'){ Set-Item "env:$Flag" '1' } else { Set-Item "env:$Flag" $null }
  $log="$here\wtfab_$($Flag)_$($i)_$cfg.log"
  # A foreign boot can claim 25599 between the check above and our bind. That is a
  # LOST run, not a slow one — retry instead of recording a bogus NONE.
  for($try=1; $try -le 4; $try++){
    robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
    $sw=[Diagnostics.Stopwatch]::StartNew()
    $p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
    $p.ProcessorAffinity=[IntPtr]$mask; $done=$false
    while($sw.Elapsed.TotalSeconds -lt $CapS){ if($p.HasExited){break}; if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){$done=$true;break}; Start-Sleep -Milliseconds 500 }
    $wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
    Start-Sleep -Milliseconds 500; if(-not $p.HasExited){Stop-Process -Id $p.Id -Force -EA SilentlyContinue}
    if($done -or -not (Select-String -Path "$log.err" -Pattern 'BindException' -Quiet)){ break }
    Write-Host ("run{0} {1}: port taken, retry {2}" -f $i,$cfg,$try)
    Start-Sleep -Seconds 60
  }
  $m=(Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1); $dt= if($m){$m.Matches.Groups[1].Value}else{'NONE'}
  $csv += "$i,$cfg,$dt,$done,$wall`n"; $csv | Out-File "$here\wt_boot_flag_ab_$Flag.csv" -Encoding utf8
  Write-Host ("run{0} {1} [{2}]: done={3}s reached={4} wall={5}s" -f $i,$cfg,$Cores,$dt,$done,$wall)
}
Set-Item "env:$Flag" $null
powercfg /setactive $orig | Out-Null
"FLAG AB DONE" | Out-File "$here\wt_boot_flag_ab_$Flag.done" -Encoding utf8

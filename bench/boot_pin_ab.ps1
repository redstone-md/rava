# Full MC-boot A/B: default-off baseline vs MIRROR+READ+PTRS, interleaved ABBAAB.
# Records Done(Xs) per run to boot_pin_ab.csv. Only kills its own rava.exe pid.
$root='D:\code\rava'; $here="$root\bench"; $exe="$root\target\release\rava.exe"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null
$seq=@('off','pin','pin','off','off','pin')   # interleaved
$csv="run,cfg,done_s,reached`n"; $i=0
foreach($cfg in $seq){
  $i++
  if($cfg -eq 'pin'){ $env:RAVA_PINNED_MIRROR='1';$env:RAVA_PINNED_READ='1';$env:RAVA_PINNED_PTRS='1' }
  else { $env:RAVA_PINNED_MIRROR=$null;$env:RAVA_PINNED_READ=$null;$env:RAVA_PINNED_PTRS=$null }
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $log="$here\bab_$($i)_$cfg.log"
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]0x0FFF; $done=$false
  while($sw.Elapsed.TotalSeconds -lt 300){ if($p.HasExited){break}; if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){$done=$true;break}; Start-Sleep -Milliseconds 500 }
  Start-Sleep -Milliseconds 500; if(-not $p.HasExited){Stop-Process -Id $p.Id -Force -EA SilentlyContinue}
  $m=(Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1); $dt= if($m){$m.Matches.Groups[1].Value}else{'NONE'}
  $csv += "$i,$cfg,$dt,$done`n"; $csv | Out-File "$here\boot_pin_ab.csv" -Encoding utf8
}
$env:RAVA_PINNED_MIRROR=$null;$env:RAVA_PINNED_READ=$null;$env:RAVA_PINNED_PTRS=$null
powercfg /setactive $orig | Out-Null
"BOOT AB DONE" | Out-File "$here\boot_pin_ab.done" -Encoding utf8

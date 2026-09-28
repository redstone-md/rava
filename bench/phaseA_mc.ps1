$root='D:\code\rava'; $here="$root\bench"; $exe="$root\target\release\rava.exe"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
$env:RAVA_JIT='1';$env:RAVA_PROC_CAP_MB='4096';$env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1';$env:RAVA_M3_BITSTORAGE='1';$env:RAVA_JIT_TIER0='1'
$seq=@('0','1','1','0','0','1','1','0')   # 4 pairs ABBA
$csv="run,pic,done_s,wall_ms,cpu_ms,reached_done`n"; $i=0
foreach($pic in $seq){
  $i++
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $env:RAVA_LAMBDA_PIC=$pic; $log="$here\mc_pa_$i.log"
  $sw=[System.Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]0x0FFF
  $done=$false
  while($sw.Elapsed.TotalSeconds -lt 320){
    if($p.HasExited){break}
    if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){$done=$true;break}
    Start-Sleep -Milliseconds 1000
  }
  $wall=[math]::Round($sw.Elapsed.TotalMilliseconds)
  $cpu=try{[math]::Round($p.TotalProcessorTime.TotalMilliseconds)}catch{0}
  if(-not $p.HasExited){Start-Sleep -Milliseconds 700}
  if(-not $p.HasExited){Stop-Process -Id $p.Id -Force -EA SilentlyContinue}
  $dt=(Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1).Matches.Groups[1].Value
  $csv += "$i,$pic,$dt,$wall,$cpu,$done`n"
  $csv | Out-File "$here\phaseA_mc.csv" -Encoding utf8
}
$env:RAVA_LAMBDA_PIC=''
"MC PHASE-A DONE" | Out-File "$here\phaseA_mc.done" -Encoding utf8

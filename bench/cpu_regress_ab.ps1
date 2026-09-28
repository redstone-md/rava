# DID THIS SESSION REGRESS THE ENGINE? Two binaries, interleaved, CPU as the signal.
#
# A fresh 3-run baseline read wall 103-143s and cpu 183-187s against a previously clean 79.3s /
# 137.6s. The wall inflation is explained by the machine not being quiet. The +35% CPU across all
# three runs is NOT obviously contention, and engine changes shipped since that baseline
# (instanceof memo default-ON, arena_str_refcell default-ON, Cell->AtomicBool, and several census
# hooks that early-return but still cost a call).
#
# WALL cannot answer this on a machine that will not go quiet - but CPU compared BETWEEN TWO
# BINARIES on the SAME machine, interleaved, can: whatever contention exists is shared by both arms
# and cancels in the delta. That is the whole reason this script exists rather than another
# wall_budget run.
#
# exe_base = ab5fb07 (the commit whose measurement produced cpu=137.6s)
# exe_head = current HEAD
param([int]$Reps=3,[int]$TimeoutS=400)
$root='D:\code\rava'; $here="$root\bench"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE','RAVA_PROFILE_PC',
              'RAVA_PROFILE_WALL','RAVA_JIT_STATS','RAVA_EXEC_THREADS','RAVA_JIT_BLOCK_CENSUS',
              'RAVA_ENUM_BIRTH_CENSUS','RAVA_ALLOC_HISTO','RAVA_CELL_CENSUS','RAVA_JIT_OPT_NONE',
              'RAVA_JIT_FLAT_TAGSELECT','RAVA_INLINE_DEPTH'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

function Run-One {
  param([string]$Exe,[string]$Tag,[int]$I)
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $log="$here\cpureg_${Tag}_$I.log"
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $Exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]0x0FFF
  while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
    if($p.HasExited){break}
    Start-Sleep -Milliseconds 400
    if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
  }
  $wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
  try { $p.Refresh(); $cpu=[math]::Round($p.TotalProcessorTime.TotalSeconds,1) } catch { $cpu=0 }
  Start-Sleep -Milliseconds 400
  if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  $m = Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1
  $done = if($m){ [double]$m.Matches.Groups[1].Value } else { -1 }
  Remove-Item $log,"$log.err" -EA SilentlyContinue
  Write-Host ("  {0,-4} rep{1}  wall={2,-7}s cpu={3,-7}s done={4}" -f $Tag,$I,$wall,$cpu,$done)
  [pscustomobject]@{ tag=$Tag; wall=$wall; cpu=$cpu; done=$done }
}

$rows=@()
for($i=1; $i -le $Reps; $i++){
  $rows += Run-One "$here\exe_base.exe" 'base' $i
  $rows += Run-One "$here\exe_head.exe" 'head' $i
}
powercfg /setactive $orig | Out-Null
Write-Host ""
foreach($tag in 'base','head'){
  $v = @($rows | Where-Object { $_.tag -eq $tag })
  $c = @($v | ForEach-Object { $_.cpu } | Sort-Object)
  $w = @($v | ForEach-Object { $_.wall } | Sort-Object)
  Write-Host ("{0}: cpu min={1} median={2} [{3}]   wall min={4}" -f $tag,$c[0],$c[[int]($c.Count/2)],($c -join '/'),$w[0])
}
$bc = @($rows | Where-Object { $_.tag -eq 'base' } | ForEach-Object { $_.cpu } | Measure-Object -Minimum).Minimum
$hc = @($rows | Where-Object { $_.tag -eq 'head' } | ForEach-Object { $_.cpu } | Measure-Object -Minimum).Minimum
Write-Host ""
Write-Host ("CPU min-of-N: base={0}s head={1}s  =>  {2}%" -f $bc,$hc,[math]::Round(100.0*($hc-$bc)/[math]::Max($bc,1),2))
Write-Host "CPU is the signal: shared contention cancels between two binaries on the same machine."

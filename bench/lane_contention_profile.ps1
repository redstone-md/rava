# WHERE DOES THE SUPERLINEAR CPU GO?
#
# The lane's own measured curve (gilfree_cf.rs worker_cap) is the reason this exists:
#
#   workers  serial    1       2       3       4       5       6
#   Done     31.258  49.393  29.954  23.451  23.414  27.872  31.127
#   CPU        91.4   105.4   113.6   120.5   138.2   170.5   202.8
#
# Identical work (identical cbs_sum) costs 2.2x the CPU at six workers than at one. That
# term is pure contention, and it is what caps Done: at 1.0 scaling, 91.4s of work over 8
# workers is ~11s, and the goal wants Done < 10.
#
# A LEAF PROFILE AT ONE WIDTH CANNOT NAME IT. A row that is 3% at one worker and 3% at six
# is innocent even if it is large; a row that is 1% at one and 12% at six IS the contention,
# however small it looked alone. So sample the SAME boot at two widths and rank by GROWTH.
#
# Reads RAVA_PROFILE_PC_STACKS (release-diag: `release` mis-names symbols). Runs the two
# arms strictly one at a time -- two concurrent boots would contend with each other and
# manufacture exactly the signal being measured.
param(
  [int]$Narrow = 1,
  [int]$Wide = 6,
  [int]$TimeoutS = 400
)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release-diag\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())

# DISARM the per-dispatch instrumentation. These are contended atomics on the dispatch path;
# leaving one armed has three times produced a bogus number, once a "128s batch" that was
# 41.8s clean. They would also be indistinguishable from the contention under study.
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_FALLBACK_TALLY'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';RAVA_PROFILE_PC_STACKS='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }

$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

function Run-Arm([int]$Threads){
  $env:RAVA_EXEC_THREADS = "$Threads"
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $log="$here\lanecon_$Threads.log"
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]0x0FFF
  while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
    if($p.HasExited){break}
    Start-Sleep -Milliseconds 500
    if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
  }
  $wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
  try { $p.Refresh(); $cpu=[math]::Round($p.TotalProcessorTime.TotalSeconds,1) } catch { $cpu=0 }
  Start-Sleep -Milliseconds 500
  if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  $m = Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1
  $done = if($m){ [double]$m.Matches.Groups[1].Value } else { -1 }
  Write-Host ("  threads={0}  wall={1}s  cpu={2}s  done={3}s" -f $Threads,$wall,$cpu,$done)
  # Leaf histogram: "[PC-STACK]  <pct>%  <count>  <symbol>", first block only per capture.
  $h=@{}
  foreach($line in (Get-Content "$log.err" -EA SilentlyContinue)){
    if($line -match '^\[PC-(?:STACK|PROFILE)\]\s+([0-9.]+)%\s+(\d+)\s+(.+)$'){
      $sym=$Matches[3].Trim(); $h[$sym] = [int]$h[$sym] + [int]$Matches[2]
    }
  }
  [pscustomobject]@{ Threads=$Threads; Wall=$wall; Cpu=$cpu; Done=$done; Hist=$h }
}

Write-Host "LANE CONTENTION PROFILE  (rank by GROWTH, not by size)"
$a = Run-Arm $Narrow
$b = Run-Arm $Wide
powercfg /setactive $orig | Out-Null

function Share($h,$k){ $tot=($h.Values | Measure-Object -Sum).Sum; if(-not $tot){return 0}; 100.0*$h[$k]/$tot }
$keys = @($a.Hist.Keys + $b.Hist.Keys) | Sort-Object -Unique
$rows = foreach($k in $keys){
  $sa=[math]::Round((Share $a.Hist $k),2); $sb=[math]::Round((Share $b.Hist $k),2)
  [pscustomobject]@{ Symbol=$k; "n$Narrow"=$sa; "n$Wide"=$sb; Growth=[math]::Round($sb-$sa,2) }
}
Write-Host ""
Write-Host ("GREW from {0} to {1} workers -- these are the contention:" -f $Narrow,$Wide)
$rows | Sort-Object Growth -Descending | Select-Object -First 20 | Format-Table -AutoSize
$rows | Sort-Object Growth -Descending | Export-Csv "$here\lane_contention.csv" -NoTypeInformation
Write-Host ("wrote {0}\lane_contention.csv" -f $here)

# A FAST REPRODUCTION OF THE LANE'S SUPERLINEAR CPU.
#
# The wall arithmetic (bench/wall_budget.ps1) says Done<10 is reachable only through the ring:
# its work is ~91 CPU-s, so at clean scaling over 8-12 cores it fits. What stops it is that the
# lane burns MORE TOTAL CPU as workers are added, for identical work -- 91.4s serial, 105.4s at
# one worker, 138.2s at four, 202.8s at six -- so Done bottoms out at ~23s and width 6 is an
# outright regression.
#
# Three hypotheses have already died: the lane LOCK is idle (RAVA_LANE_OUTER, top holder
# hold_ms=1), the top growth row was an innocent allocator realloc (fixing it was neutral at
# width 4 and WORSE at width 6), and dispatch callbacks turned out to be nearly free (removing
# 35M of them bought zero time). Each of those cost a ~100s boot per data point, which is why
# the search has been so slow.
#
# So build the missing tool instead of guessing again: RingLane is the only gate that ARMS the
# lane, it runs in seconds, and if the CPU inflation reproduces there then the whole question
# becomes bisectable at ~20x the iteration rate.
#
# CPU, not wall, is the signal. A cores/s plot cannot see this: wall improves while CPU
# balloons, which is exactly what "buying wall with contention" looks like.
# AFFINITY MATTERS AND IS THE FIRST THING TO RULE OUT. 0x0FFF is 12 LOGICAL cpus = 6 PHYSICAL
# cores with hyperthreading; past ~5 workers the lane necessarily lands on HT siblings, which
# share execution units and inflate CPU-TIME for identical work as a matter of physics, not
# contention. -Mask 0x0555 picks one logical cpu per physical core (0,2,4,6,8,10) so the two
# causes can be told apart.
param([int]$N=8,[string]$Threads='1,2,4,6,8',[int]$Reps=2,[string]$Mask='0x0FFF')
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp="$here\out;"+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
# Every per-dispatch diagnostic disarmed: each is a contended atomic and would manufacture
# precisely the signal under study. This has produced a bogus number three times already.
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE',
              'RAVA_PROFILE_PC','RAVA_PROFILE_PC_STACKS','RAVA_PROFILE_WALL','RAVA_JIT_STATS'){
  Remove-Item "env:$k" -EA SilentlyContinue
}
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

$rows=@()
foreach($t in $Threads.Split(',')){
  $best=$null
  for($r=1; $r -le $Reps; $r++){
    $env:RAVA_EXEC_THREADS="$t"
    $log="$here\lanecpu_${t}_$r.log"
    $sw=[Diagnostics.Stopwatch]::StartNew()
    $p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'RingLane',"$N") -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
    # [int]'0x0FFF' does NOT parse in PowerShell - it throws, and a silently wrong affinity
    # would invalidate the whole comparison. Convert explicitly from hex.
    $p.ProcessorAffinity=[IntPtr][Convert]::ToInt64($Mask.Replace('0x',''),16)
    $p.WaitForExit(600000) | Out-Null
    $wall=[math]::Round($sw.Elapsed.TotalSeconds,2)
    try { $p.Refresh(); $cpu=[math]::Round($p.TotalProcessorTime.TotalSeconds,2) } catch { $cpu=0 }
    $txt = Get-Content $log -Raw -EA SilentlyContinue
    $ms = if($txt -match 'LANE_MS=(\d+)'){ [int]$Matches[1] } else { -1 }
    $hash = if($txt -match 'HASH=([0-9a-f]+)'){ $Matches[1] } else { '?' }
    if(-not $best -or $cpu -lt $best.cpu){ $best=[pscustomobject]@{t=[int]$t;wall=$wall;cpu=$cpu;ms=$ms;hash=$hash} }
    Remove-Item $log,"$log.err" -EA SilentlyContinue
  }
  $rows += $best
  Write-Host ("threads={0,-2}  lane_ms={1,-7} wall={2,-6}s cpu={3,-7}s  hash={4}" -f $best.t,$best.ms,$best.wall,$best.cpu,$best.hash)
}
$env:RAVA_EXEC_THREADS=$null
powercfg /setactive $orig | Out-Null

$b = $rows | Sort-Object t | Select-Object -First 1
# ISOLATE THE LANE. Whole-process CPU is dominated by ~43s of identical setup in every arm,
# which dilutes the effect roughly tenfold and made the first run of this script look almost
# flat. That setup CPU is the same in all arms, so the DELTA is entirely the lane, and the
# narrowest arm's lane CPU is its wall (it is single-threaded there).
$laneBase = $b.ms / 1000.0
Write-Host ""
Write-Host ("LANE CPU (derived: lane_cpu_1 = lane_ms_1 = {0}s; lane_cpu_N = that + (cpu_N - cpu_1))" -f $laneBase)
Write-Host "identical work in every arm, so any rise is pure contention:"
foreach($r in ($rows | Sort-Object t)){
  $lane = $laneBase + ($r.cpu - $b.cpu)
  $infl = if($laneBase -gt 0){ [math]::Round($lane/$laneBase,2) } else { 0 }
  Write-Host ("  threads={0,-2}  lane_ms={1,-7} lane_cpu={2,6:N2}s  x{3}" -f $r.t,$r.ms,$lane,$infl)
}
$rows | Sort-Object t | Export-Csv "$here\lane_cpu_curve.csv" -NoTypeInformation
$hashes = ($rows | Select-Object -ExpandProperty hash | Sort-Object -Unique)
Write-Host ""
if($hashes.Count -eq 1){ Write-Host ("all arms bit-exact: {0}" -f $hashes[0]) }
else { Write-Host ("*** HASHES DIFFER ACROSS ARMS: {0} ***" -f ($hashes -join ' ')) -ForegroundColor Red }
Write-Host "(expected 37539c6c375d0c3)"

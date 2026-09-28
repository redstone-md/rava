# INTERLEAVED TWO-BINARY A/B, WITH THE OUTCOME CHECK.
#
# Why two binaries rather than one binary and a flag: a flag that must be READ costs something, and
# for changes that REMOVE a read (an env gate hoisted behind a OnceLock) there is no honest flagged
# arm - the kill switch would reintroduce the very cost under test. Two binaries also cancel shared
# machine contention in the delta, which is the only way to get a signal on a box that will not go
# quiet.
#
# NON-NEGOTIABLE: a run that does not reach `Done (...)` is NOT a measurement. Two sweeps in this
# tree once reported eighteen tidy wall/cpu rows that were all times-to-failure, and an afternoon of
# arithmetic was built on them. `Assert-FreshExe` cannot substitute - it compares mtimes and passes
# a binary built recently from an old commit.
#
# Order is PERMUTED per rep. Plain alternation leaves each arm at a fixed offset inside the rep, so
# anything periodic on the machine lands on the same arm every time, and a monotonically quieting
# box makes whichever arm runs later look better.
param(
  [Parameter(Mandatory=$true)][string]$ExeA,
  [Parameter(Mandatory=$true)][string]$ExeB,
  [string]$NameA='A',
  [string]$NameB='B',
  [int]$Reps=4,
  [int]$TimeoutS=400,
  # CONTENTION FLOOR. A run whose wall exceeds this is not comparable to one that did not wait on
  # somebody else's compiler; it is excluded and the pair is retried. 0 disables the check.
  #
  # This exists because the expensive failure on this box is not a wrong number, it is a WASTED
  # BATCH: half the A/Bs in the session that produced this harness came back "OVERLAP - a
  # direction, not a verdict" purely because the machine turned mid-run, and each one cost 15
  # minutes and a turn. Retrying until enough CLEAN pairs exist converts that into wall-clock
  # instead of into an inconclusive answer.
  [double]$CleanWallS=0,
  # Stop retrying after this many EXTRA pairs, so a permanently busy box fails fast and loudly
  # rather than looping.
  [int]$MaxExtraPairs=6
)
$root='D:\code\rava'; $here="$root\bench"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE','RAVA_PROFILE_PC',
              'RAVA_PROFILE_WALL','RAVA_JIT_STATS','RAVA_EXEC_THREADS','RAVA_JIT_BLOCK_CENSUS',
              'RAVA_CELL_CENSUS','RAVA_JIT_THRESHOLD','RAVA_COMPILE_TIME','RAVA_FLAT_GETFIELD',
              'RAVA_PROFILE_PC_STACKS','RAVA_RESTORE','RAVA_CHECKPOINT_FILE','RAVA_CHECKPOINT_OPS',
              'RAVA_STACKFUL'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';
        RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
foreach($e in @($ExeA,$ExeB)){ if(-not (Test-Path $e)){ Write-Host "missing $e"; exit 1 } }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

function Run-One {
  param([string]$Exe,[string]$Tag,[int]$I)
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $log="$here\abrun_${Tag}_$I.log"
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $Exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
      -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]0x0FFF
  while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
    if($p.HasExited){break}
    Start-Sleep -Milliseconds 400
    if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
  }
  $wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
  # PEAK RSS, sampled before the kill. A change can be slower for a reason the timing alone cannot
  # name - a per-class table that trades a hash for an allocation shows up here first - and this
  # was added while attributing exactly such a regression.
  try { $p.Refresh(); $cpu=[math]::Round($p.TotalProcessorTime.TotalSeconds,1) } catch { $cpu=0 }
  try { $rss=[math]::Round($p.PeakWorkingSet64/1MB) } catch { $rss=0 }
  Start-Sleep -Milliseconds 400
  if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  # WAIT FOR THE PROCESS TO ACTUALLY GO, then settle. `Stop-Process` returns before the kernel has
  # torn the process down and released its listening socket, and the NEXT boot then dies in under a
  # second binding the port. That failure looked like a bad binary until the exe was smoke-tested.
  try { $p.WaitForExit(5000) | Out-Null } catch {}
  Start-Sleep -Milliseconds 900
  $m = Select-String -Path $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1
  $done = -1.0
  if($m){ $done = [double]$m.Matches.Groups[1].Value }
  if($done -lt 0){
    $why = Select-String -Path $log,"$log.err" -Pattern `
             'IllegalStateException|BindException|Address already in use|already in use|uncaught|Exception|panicked|proceed with server load' |
           Select-Object -First 1
    $t='no Done line and no recognised error'
    if($why){ $t = ($why.Line -replace '^\s+','') }
    if($t.Length -gt 110){ $t = $t.Substring(0,110) }
    # KEEP THE EVIDENCE. Deleting the log of a failed run left "no recognised error" as the only
    # trace, which is unfixable after the fact - the whole point of the outcome check is to make a
    # non-measurement diagnosable, not merely to exclude it.
    $keep="$here\abfail_${Tag}_$I.log"
    Move-Item $log $keep -Force -EA SilentlyContinue
    Move-Item "$log.err" "$keep.err" -Force -EA SilentlyContinue
    Write-Host ("  {0,-6} rep{1}  *** FAILED after {2}s - NOT A MEASUREMENT" -f $Tag,$I,$wall)
    Write-Host ("          {0}" -f $t)
    Write-Host ("          log kept: {0}" -f $keep)
    return [pscustomobject]@{ tag=$Tag; wall=-1; cpu=$cpu; done=-1; rss=$rss; ok=$false }
  }
  Remove-Item $log,"$log.err" -EA SilentlyContinue
  Write-Host ("  {0,-6} rep{1}  wall={2,-7}s Done={3,-9}s cpu={4,-6}s rss={5}MB" -f $Tag,$I,$wall,$done,$cpu,$rss)
  [pscustomobject]@{ tag=$Tag; wall=$wall; cpu=$cpu; done=$done; rss=$rss; ok=$true }
}

$rows=@()
$pairDeltas=@()
$i=0; $extra=0
while($true){
  $i++
  # BALANCED ALTERNATION, not `Sort-Object { Get-Random }`. Random order does not balance on a
  # handful of reps - it can easily deal the same order three or four times running, and the FIRST
  # run of a pair is systematically slower (the world restore just finished, page cache is cold,
  # the CPU has not settled). An A/A CONTROL with the identical binary in both arms proved it:
  #
  #   ctrlA  Done 21.795 22.168 22.504 22.537  mean 22.25   wall 71.3 71.6 72.0 72.5
  #   ctrlB  Done 22.447 22.451 22.663 23.130  mean 22.67   wall 72.1 72.1 72.6 72.9
  #
  # -1.85% on Done means, -2.9% on minima, and the "winner" ahead in EVERY sorted position -
  # produced entirely by the arm that happened to run second more often. Strict alternation makes
  # first-position count equal for both arms, which is the only thing that cancels it.
  $pair = if($i % 2 -eq 1){
    @(@{n=$NameA;e=$ExeA}, @{n=$NameB;e=$ExeB})
  } else {
    @(@{n=$NameB;e=$ExeB}, @{n=$NameA;e=$ExeA})
  }
  Write-Host ("--- rep $i  (order: " + (($pair | ForEach-Object { $_.n }) -join ',') + ") ---")
  $got=@()
  foreach($x in $pair){ $got += Run-One $x.e $x.n $i }
  $rows += $got
  # PAIRED DELTA. The two arms of a rep run minutes apart and see almost the same machine, so the
  # WITHIN-PAIR difference cancels drift that the per-arm means cannot. This box does not go quiet
  # - one pair in this session read base=114.3s against sc=83.8s purely from background load - and
  # comparing means across such runs is how an afternoon gets spent on "OVERLAP, a direction".
  # A FAILED ARM VOIDS THE WHOLE PAIR. Keeping the surviving arm leaves the batch unbalanced, and
  # an unbalanced batch is exactly what the A/A control caught manufacturing a 1.85% "win" out of
  # nothing - so dropping only the failed run would reintroduce the bias this harness exists to
  # remove.
  if(@($got | Where-Object { $_.ok }).Count -ne 2){
    foreach($g in $got){ $g | Add-Member -NotePropertyName dirty -NotePropertyValue $true -Force }
    Write-Host "    pair VOIDED (an arm failed) - retrying"
    $extra++
    if($extra -gt $MaxExtraPairs){
      Write-Host "*** gave up - too many failed runs" -ForegroundColor Red
      break
    }
    continue
  }
  $pa = $got | Where-Object { $_.tag -eq $NameA -and $_.ok }
  $pb = $got | Where-Object { $_.tag -eq $NameB -and $_.ok }
  if($pa -and $pb){
    $pairDeltas += [pscustomobject]@{
      rep=$i; dDone=[math]::Round($pa.done-$pb.done,3); dWall=[math]::Round($pa.wall-$pb.wall,1)
      dirty=($CleanWallS -gt 0 -and (($pa.wall -gt $CleanWallS) -or ($pb.wall -gt $CleanWallS)))
    }
  }
  # A PAIR counts only if BOTH arms of it are clean: comparing a clean run against a contended one
  # is exactly the mistake this guard exists to stop, and dropping only the slow arm would do that.
  if($CleanWallS -gt 0){
    $pairClean = ($got | Where-Object { $_.ok -and $_.wall -le $CleanWallS }).Count -eq 2
    if(-not $pairClean){
      foreach($g in $got){ $g | Add-Member -NotePropertyName dirty -NotePropertyValue $true -Force }
      Write-Host ("    pair discarded (a run exceeded {0}s) - retrying" -f $CleanWallS)
      $extra++
      if($extra -gt $MaxExtraPairs){
        Write-Host ("*** gave up after $MaxExtraPairs extra pairs - the box is not quiet enough to A/B") -ForegroundColor Red
        break
      }
      continue
    }
  }
  # Dirty covers BOTH rejections - exceeded the wall floor, or lost an arm - so it is filtered
  # unconditionally. Counting voided rows when -CleanWallS is 0 would let a batch "reach" its rep
  # target on pairs it had already thrown away.
  $cleanPairs = [math]::Floor((@($rows | Where-Object { $_.ok -and -not $_.dirty }).Count) / 2)
  if($cleanPairs -ge $Reps){ break }
}
$rows = @($rows | Where-Object { -not $_.dirty })
powercfg /setactive $orig | Out-Null

Write-Host ""
$stat=@{}
foreach($n in @($NameA,$NameB)){
  $v = @($rows | Where-Object { $_.tag -eq $n -and $_.ok })
  if(-not $v){ Write-Host ("{0}: *** EVERY RUN FAILED" -f $n); continue }
  $w = @($v | ForEach-Object { $_.wall } | Sort-Object)
  $d = @($v | ForEach-Object { $_.done } | Sort-Object)
  $c = @($v | ForEach-Object { $_.cpu }  | Sort-Object)
  $r = @($v | ForEach-Object { $_.rss }  | Sort-Object)
  $stat[$n]=[pscustomobject]@{
    n=$v.Count
    wmin=$w[0]; wmean=[math]::Round((($w | Measure-Object -Average).Average),2); wmax=$w[-1]
    dmin=$d[0]; dmean=[math]::Round((($d | Measure-Object -Average).Average),2); dmax=$d[-1]
    cmean=[math]::Round((($c | Measure-Object -Average).Average),1)
    rmean=[math]::Round((($r | Measure-Object -Average).Average),0)
  }
  Write-Host ("{0,-8} n={1}  wall min/mean {2}/{3}  Done min/mean {4}/{5}  cpu mean {6}  rss mean {7}MB" -f `
    $n,$v.Count,$w[0],$stat[$n].wmean,$d[0],$stat[$n].dmean,$stat[$n].cmean,$stat[$n].rmean)
  Write-Host ("{0,-8} wall runs: {1}" -f '',($w -join ' '))
  Write-Host ("{0,-8} Done runs: {1}" -f '',($d -join ' '))
}
if($stat.ContainsKey($NameA) -and $stat.ContainsKey($NameB)){
  $a=$stat[$NameA]; $b=$stat[$NameB]
  Write-Host ""
  Write-Host ("Done  mean {0} vs {1}: {2}%   |  wall mean: {3}%" -f `
    $a.dmean,$b.dmean,[math]::Round(100.0*($a.dmean-$b.dmean)/[math]::Max($b.dmean,0.01),2), `
    [math]::Round(100.0*($a.wmean-$b.wmean)/[math]::Max($b.wmean,0.01),2))
  # ON CLEAN = every run of one arm beats every run of the other. Anything less is a direction.
  $cleanA = ($a.dmax -lt $b.dmin); $cleanB = ($b.dmax -lt $a.dmin)
  if($cleanA){ Write-Host ("=> {0} beats {1} ON CLEAN on Done (every run below every run)" -f $NameA,$NameB) }
  elseif($cleanB){ Write-Host ("=> {0} beats {1} ON CLEAN on Done" -f $NameB,$NameA) }
  else { Write-Host "=> OVERLAP on Done - a direction, not a verdict" }
}

# PAIRED VIEW. Reported alongside the per-arm means, never instead of them: it is the estimator
# that survives a drifting box, but it says nothing about MAGNITUDE stability, so a large mean gap
# with an inconsistent sign is still noise and both views have to be read together.
$pd = @($pairDeltas)
if($pd.Count -ge 2){
  Write-Host ""
  Write-Host ("PAIRED (within-rep {0} minus {1}; negative = {0} faster):" -f $NameA,$NameB)
  foreach($d in $pd){
    Write-Host ("  rep{0}  dDone={1,-8}  dWall={2,-6}{3}" -f $d.rep,$d.dDone,$d.dWall,$(if($d.dirty){'  (dirty pair)'}else{''}))
  }
  $dd = @($pd | ForEach-Object { $_.dDone } | Sort-Object)
  $med = if($dd.Count % 2 -eq 1){ $dd[[math]::Floor($dd.Count/2)] } else { ($dd[$dd.Count/2-1]+$dd[$dd.Count/2])/2 }
  $negs = @($pd | Where-Object { $_.dDone -lt 0 }).Count
  $n = $pd.Count
  Write-Host ("  median dDone = {0}s over {1} pairs; {2}/{1} favour {3}" -f `
    [math]::Round($med,3),$n,$(if($negs*2 -ge $n){$negs}else{$n-$negs}),$(if($negs*2 -ge $n){$NameA}else{$NameB}))
  # SIGN TEST. All-one-way is the only outcome worth calling on this few pairs: 4 pairs one way is
  # p=1/8 two-sided, 5 is 1/16, 6 is 1/32. Anything mixed is explicitly NOT a verdict.
  if($negs -eq $n -and $n -ge 4){ Write-Host ("  => every pair favours {0} (sign test p={1:N3})" -f $NameA,[math]::Pow(0.5,$n-1)) }
  elseif($negs -eq 0 -and $n -ge 4){ Write-Host ("  => every pair favours {0} (sign test p={1:N3})" -f $NameB,[math]::Pow(0.5,$n-1)) }
  else { Write-Host "  => signs are MIXED - no paired verdict" }
}

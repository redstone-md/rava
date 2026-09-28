# IS THE TIER-0 COMPILE THRESHOLD (16) PAST ITS OPTIMUM?
#
# `jit_threshold` is invocations-before-compile. Tier-0 sets it to 16, plain default is 512, and
# RAVA_JIT_TIER0 was measured at -40% - so 16 beat 512 decisively and NOTHING BETWEEN THEM WAS EVER
# TRIED. That is the exact shape this project has already paid for once: the ring lane's hardcoded
# worker count 6 sat past its optimum, and the curve (serial 31.26 / 1w 49.39 / 2w 29.95 / 3w 23.45
# / 4w 23.41 / 5w 27.87 / 6w 31.13) only appeared when someone swept it instead of trusting the
# constant that had won its original A/B.
#
# The knob trades two measured masses against each other, both inside the SERIAL 68% of wall:
#   cranelift-compile  ~4.3s  - 13.9% of serial JIT_EXEC, us compiling ON the critical path
#   INTERP_EXEC        ~6s    - 10.8% of serial wall, frames that never reached the threshold
# Raising it compiles fewer bodies (less compile time, more interpretation); lowering it does the
# reverse and risks exhausting `jit_cap`, which is the documented failure at threshold 10.
#
# So each arm reports THREE numbers, not one: wall, Done, and the [COMPILE-TIME] readout
# (RAVA_COMPILE_TIME, cumulative Cranelift seconds + methods compiled). If a threshold wins, the
# compile line says whether it won by compiling less or by compiling better.
#
# ROUND-ROBIN, not blocked: every value is visited once per rep, so machine drift lands on all arms
# alike. On a contended box the ABSOLUTE wall is inflated and uninterpretable; the ORDERING across
# arms measured in the same rep is not - the same argument that made the two-binary CPU comparison
# valid. Reported as min-of-N per arm.
#
# This is a SHAPE pass. A winner here is a candidate, not a verdict: confirm it against 16 with more
# reps on a quiet machine before shipping a default.
param([int[]]$Thresholds=@(16,32,64,128,256),[int]$Reps=2,[int]$TimeoutS=420)
$root='D:\code\rava'; $here="$root\bench"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
# Diagnostics are contended per-dispatch atomics - they have confounded three verdicts in this
# project. Disarm explicitly rather than assuming a clean environment.
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE','RAVA_PROFILE_PC',
              'RAVA_PROFILE_WALL','RAVA_JIT_STATS','RAVA_EXEC_THREADS','RAVA_JIT_BLOCK_CENSUS',
              'RAVA_ENUM_BIRTH_CENSUS','RAVA_ALLOC_HISTO','RAVA_CELL_CENSUS','RAVA_JIT_OPT_NONE',
              'RAVA_JIT_FLAT_TAGSELECT','RAVA_INLINE_DEPTH','RAVA_FLAT_GETFIELD',
              'RAVA_TRACE_COMPILE'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';
        RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';RAVA_COMPILE_TIME='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$exe="$root\target\release\rava.exe"
if(-not (Test-Path $exe)){ Write-Host "missing $exe - build: cargo build --release -p rava-cli --features next-gen-gc"; exit 1 }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

function Run-One {
  param([int]$Thr,[int]$I)
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $env:RAVA_JIT_THRESHOLD="$Thr"
  $log="$here\thrsweep_${Thr}_$I.log"
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
      -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
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
  # [COMPILE-TIME] prints every 500 methods -> the LAST line is the high-water mark reached.
  $ct = Select-String "$log.err" -Pattern '\[COMPILE-TIME\] (\d+) methods compiled, ([0-9.]+)s' | Select-Object -Last 1
  $meth=0; $csec=0.0
  if($ct){ $meth=[int]$ct.Matches.Groups[1].Value; $csec=[double]$ct.Matches.Groups[2].Value }
  # Keep ONE log whenever Done is missing. The first sweep printed done=-1 on all ten runs and the
  # evidence was deleted each time, so nothing could be checked.
  # ⚠ The capture is CORRECT and was never the bug: a real boot prints exactly `Done (25.963s)`,
  # which `Done \(([0-9.]+)s\)` matches. The wait loop did not match it either - it exited through
  # `$p.HasExited`, because every one of those runs died before Done. A missing Done means a FAILED
  # RUN, not a parsing problem, which is why the retained log matters more than the regex.
  $fail = ($done -lt 0)
  if($fail -and -not (Test-Path "$here\thrsweep_DONEPARSE.log")){
    Copy-Item $log "$here\thrsweep_DONEPARSE.log" -EA SilentlyContinue
  }
  # A RUN THAT DID NOT REACH Done IS NOT A MEASUREMENT. The first two sweeps reported ten and eight
  # tidy wall/cpu rows that were all times-to-FAILURE: the binary aborted in datapack loading with
  # "Failed to parse <arena-object@...> from pack vanilla", and because the harness only ever
  # printed numbers, a whole afternoon of arithmetic was done on top of them. Note this cannot be
  # delegated to Assert-FreshExe - that guard compares mtimes and explicitly passes a binary built
  # RECENTLY from an OLD commit, which is exactly what had been hardlinked into target\release.
  if($fail){
    $why = Select-String -Path $log -Pattern 'IllegalStateException|BindException|uncaught|can''t proceed' |
           Select-Object -First 1
    $whyTxt = if($why){ ($why.Line -replace '^\s+','').Substring(0,[Math]::Min(110,$why.Line.Length)) } else { 'no Done line, no recognised error' }
  }
  Remove-Item $log,"$log.err" -EA SilentlyContinue
  if($fail){
    Write-Host ("  thr={0,-4} rep{1}  *** FAILED after {2}s - NOT A MEASUREMENT" -f $Thr,$I,$wall) -ForegroundColor Red
    Write-Host ("           {0}" -f $whyTxt)
    return [pscustomobject]@{ thr=$Thr; wall=-1; done=-1; cpu=$cpu; meth=$meth; csec=$csec; ok=$false }
  }
  Write-Host ("  thr={0,-4} rep{1}  wall={2,-7}s done={3,-7} cpu={4,-7}s  compiled>={5} in {6}s" -f `
    $Thr,$I,$wall,$done,$cpu,$meth,$csec)
  [pscustomobject]@{ thr=$Thr; wall=$wall; done=$done; cpu=$cpu; meth=$meth; csec=$csec; ok=$true }
}

$rows=@()
for($i=1; $i -le $Reps; $i++){
  # PERMUTE the order every rep. Round-robin alone does NOT protect against periodic background
  # load: with a fixed order each arm always runs at the same offset within the rep, so anything
  # periodic on the box lands on the same arm every time. The first run of this sweep showed
  # exactly that shape - thr=32 was the worst arm in both reps while its neighbours 16/64/128 sat
  # within 3s of each other - which is indistinguishable from a real effect and is not one.
  $order = $Thresholds | Sort-Object { Get-Random }
  Write-Host ("--- rep $i of $Reps  (order: " + ($order -join ',') + ") ---")
  foreach($t in $order){ $rows += Run-One $t $i }
}
Remove-Item env:RAVA_JIT_THRESHOLD -EA SilentlyContinue
powercfg /setactive $orig | Out-Null

Write-Host ""
Write-Host "threshold   wall(min)   Done(min)   cpu(min)   methods   compile-s"
$best=$null
foreach($t in $Thresholds){
  # Filter on WALL, never on Done: Done is parsed out of MC's own log line and a parse miss must
  # not delete an otherwise valid run from the summary. The first version filtered on `done -gt 0`
  # and would have discarded EVERY row of a 25-minute sweep because of one bad regex.
  $v = @($rows | Where-Object { $_.thr -eq $t -and $_.ok })
  if(-not $v){ Write-Host ("{0,-11} *** EVERY RUN FAILED - no measurement for this arm" -f $t); continue }
  $dv = @($v | Where-Object { $_.done -gt 0 })
  $w = ($v | ForEach-Object { $_.wall } | Measure-Object -Minimum).Minimum
  $d = if($dv){ ($dv | ForEach-Object { $_.done } | Measure-Object -Minimum).Minimum } else { 'n/a' }
  $c = ($v | ForEach-Object { $_.cpu }  | Measure-Object -Minimum).Minimum
  $me= ($v | ForEach-Object { $_.meth } | Measure-Object -Maximum).Maximum
  $cs= ($v | ForEach-Object { $_.csec } | Measure-Object -Maximum).Maximum
  Write-Host ("{0,-11} {1,-11} {2,-11} {3,-10} {4,-9} {5}" -f $t,$w,$d,$c,$me,$cs)
  if($null -eq $best -or $w -lt $best.w){ $best=[pscustomobject]@{t=$t;w=$w;d=$d} }
}
$b16 = @($rows | Where-Object { $_.thr -eq 16 -and $_.done -gt 0 })
if($b16 -and $best){
  $w16 = ($b16 | ForEach-Object { $_.wall } | Measure-Object -Minimum).Minimum
  $d16 = ($b16 | ForEach-Object { $_.done } | Measure-Object -Minimum).Minimum
  Write-Host ""
  Write-Host ("best={0}: wall {1}s vs thr16 {2}s ({3}%)  |  Done {4}s vs {5}s ({6}%)" -f `
    $best.t,$best.w,$w16,[math]::Round(100.0*($best.w-$w16)/[math]::Max($w16,0.1),2), `
    $best.d,$d16,[math]::Round(100.0*($best.d-$d16)/[math]::Max($d16,0.1),2))
}
Write-Host ""
Write-Host "SHAPE PASS. A winner is a candidate: confirm vs 16 with more reps on a quiet machine."
Write-Host "Absolute wall on a contended box is uninterpretable; the ORDERING within a rep is not."

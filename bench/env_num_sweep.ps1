# SWEEP ONE NUMERIC ENV KNOB OVER A REAL BOOT.
#
# Generalises jit_threshold_sweep.ps1, which was the same harness hardcoded to RAVA_JIT_THRESHOLD.
# Everything it learned the hard way is kept:
#
#   * A RUN THAT DOES NOT REACH `Done (...)` IS NOT A MEASUREMENT. Two sweeps in this tree once
#     reported eighteen tidy wall/cpu rows that were all times-to-failure, and an afternoon of
#     arithmetic was built on them. A failed run is printed with its reason and EXCLUDED, never
#     averaged.
#   * ORDER IS PERMUTED PER REP. Plain round-robin leaves each arm at a fixed offset inside the rep,
#     so anything periodic on the box lands on the same arm every time, and a monotonically
#     quieting machine makes whichever arm runs later look better.
#   * Diagnostics are disarmed explicitly - the per-dispatch atomics have confounded three verdicts
#     here - and so are the opt-OUTs of the current defaults, because a stray one inherited from a
#     shell would sweep a different engine than the one being shipped.
#
#   .\env_num_sweep.ps1 -Var RAVA_EXEC_THREADS -Values 3,4,5,6 -Reps 2
param(
  [Parameter(Mandatory=$true)][string]$Var,
  # A STRING, split below - NOT [int[]]. Invoked through `-File`, PowerShell does not parse
  # argument literals: `-Values 3,4,5,6` arrives as the single string "3,4,5,6" and coerces to
  # the integer 3456, which this harness dutifully ran as a worker count (cpu 376s, Done 45s)
  # and reported as a one-row sweep. Same limitation that silently broke `-Envs @{...}` on
  # wt_gates.ps1. Taking a string and splitting it makes the harness behave identically under
  # -File and -Command.
  [Parameter(Mandatory=$true)][string]$Values,
  [int]$Reps=2,
  [int]$TimeoutS=420
)
$root='D:\code\rava'; $here="$root\bench"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
$vals = @($Values -split '[,;\s]+' | Where-Object { $_ -ne '' } | ForEach-Object { [int]$_ })
if($vals.Count -eq 0){ Write-Host "no values parsed from '$Values'"; exit 1 }
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE','RAVA_PROFILE_PC',
              'RAVA_PROFILE_WALL','RAVA_JIT_STATS','RAVA_JIT_BLOCK_CENSUS','RAVA_VECGROW_STAT',
              'RAVA_CELL_CENSUS','RAVA_COMPILE_TIME','RAVA_FLAT_GETFIELD','RAVA_ITER_SHORTCUT_STATS',
              'RAVA_NO_ARGBUF_RECYCLE','RAVA_NO_JIT_ITER_SHORTCUT','RAVA_NO_INSTANCEOF_MEMO',
              'RAVA_EXEC_THREADS','RAVA_JIT_THRESHOLD'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';
        RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$exe="$root\target\release\rava.exe"
if(-not (Test-Path $exe)){ Write-Host "missing $exe"; exit 1 }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

function Run-One {
  param([int]$Val,[int]$I)
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  Set-Item "env:$Var" "$Val"
  $log="$here\envsweep_${Val}_$I.log"
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
  $m = Select-String -Path $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1
  $done = -1.0
  if($m){ $done = [double]$m.Matches.Groups[1].Value }
  if($done -lt 0){
    $why = Select-String -Path $log -Pattern 'IllegalStateException|BindException|uncaught|proceed with server load' |
           Select-Object -First 1
    $t='no Done line and no recognised error'
    if($why){ $t = ($why.Line -replace '^\s+','') }
    if($t.Length -gt 110){ $t = $t.Substring(0,110) }
    Remove-Item $log,"$log.err" -EA SilentlyContinue
    Write-Host ("  {0}={1,-4} rep{2}  *** FAILED after {3}s - NOT A MEASUREMENT" -f $Var,$Val,$I,$wall)
    Write-Host ("          {0}" -f $t)
    return [pscustomobject]@{ val=$Val; wall=-1; cpu=$cpu; done=-1; ok=$false }
  }
  Remove-Item $log,"$log.err" -EA SilentlyContinue
  Write-Host ("  {0}={1,-4} rep{2}  wall={3,-7}s Done={4,-9}s cpu={5}s" -f $Var,$Val,$I,$wall,$done,$cpu)
  [pscustomobject]@{ val=$Val; wall=$wall; cpu=$cpu; done=$done; ok=$true }
}

$rows=@()
for($i=1; $i -le $Reps; $i++){
  $order = $vals | Sort-Object { Get-Random }
  Write-Host ("--- rep $i of $Reps  (order: " + ($order -join ',') + ") ---")
  foreach($v in $order){ $rows += Run-One $v $i }
}
Remove-Item "env:$Var" -EA SilentlyContinue
powercfg /setactive $orig | Out-Null

Write-Host ""
Write-Host ("{0,-8} {1,-11} {2,-11} {3,-10} runs" -f $Var,'Done(min)','wall(min)','cpu(min)')
foreach($v in $vals){
  $x = @($rows | Where-Object { $_.val -eq $v -and $_.ok })
  if(-not $x){ Write-Host ("{0,-8} *** EVERY RUN FAILED" -f $v); continue }
  $d = @($x | ForEach-Object { $_.done } | Sort-Object)
  $w = @($x | ForEach-Object { $_.wall } | Sort-Object)
  $c = @($x | ForEach-Object { $_.cpu }  | Sort-Object)
  Write-Host ("{0,-8} {1,-11} {2,-11} {3,-10} Done: {4}" -f $v,$d[0],$w[0],$c[0],($d -join ' '))
}
Write-Host ""
Write-Host "Minima are the robust statistic here: contention can only inflate a run, never shorten it."
Write-Host "A winner is a CANDIDATE - confirm it against the current default with a dedicated A/B."

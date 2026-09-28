# WHICH SYMBOL HOLDS THE LANE LOCK -- at a width where the contention is large.
#
# The lane's CPU is superlinear in worker count (117.5s at one worker, 210.7s at six, for
# identical work) and width 6 is a REGRESSION on Done, not a diminishing return. Ranking host
# samples by growth pointed at an allocator row that turned out to be innocent
# (bench/lane_contention_profile.ps1 + the refutation at Vm::jit_run), which is the expected
# failure mode: a worker SPINNING ON OR WAITING FOR A LOCK burns CPU without progress and does
# not surface as its own leaf symbol.
#
# RAVA_LANE_OUTER answers the question directly -- it ranks OUTER lock acquisitions by total
# held time and names the callee that anchors each serialized span. The last time that question
# was asked it produced BiomeSource.findBiomeHorizontal holding 55s of a 77s batch, and chasing
# why led to the 32-slot call buffer and a -14% wall / -31% Done win.
#
# THIS RUN'S TIMINGS ARE NOT A MEASUREMENT. LANE_OUTER is a contended per-dispatch atomic; it
# has three separate times produced a bogus number here, once a "128s batch" that was 41.8s
# clean. Read the RANKING, never the wall.
param([int]$Threads=6,[int]$TimeoutS=600)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
# RAVA_LANE_PROBE must be armed TOO: the [LANE-OUTER] ranking is printed by the probe's
# reporter, not by the attribution module itself, so LANE_OUTER alone collects the tally and
# never dumps it. (Cost me one full instrumented boot.) The period is the probe's sample
# interval in ms -- keep it coarse, the ranking is cumulative and the samples are just noise.
Remove-Item "env:RAVA_FORK_STATS" -EA SilentlyContinue
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_LANE_OUTER='1';RAVA_LANE_PROBE='5000';RAVA_EXEC_THREADS="$Threads"}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\laneouter.log"
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
$sw=[Diagnostics.Stopwatch]::StartNew()
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 500
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
Start-Sleep -Milliseconds 600
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
Write-Host ("(instrumented run, {0}s wall -- ranking only, NOT a timing)" -f [math]::Round($sw.Elapsed.TotalSeconds,1))
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'LANE-OUTER' | Select-Object -Last 8
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'GILFREE-RINGS' | Select-Object -Last 3

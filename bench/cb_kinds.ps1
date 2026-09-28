# WHAT ARE THE 384 MILLION CALLBACKS?
#
# The ring batch reports cbs_sum=384,117,415 -- ~1.91M compiled->host transitions per task,
# 127 tasks. At the recorded ~99ns for a warm host fall that is ~38s of pure transition cost,
# which is larger than the entire ring window. It also explains the shape nothing else has:
# the lane is host-bound (only ~6% of A+B runs inside compiled code), its CPU is superlinear
# in worker count, and its LOCK is now provably idle (RAVA_LANE_OUTER at width 6: top holder
# hold_ms=1, ~2ms across the whole top-15). Callbacks are the remaining candidate, and they
# are work to REMOVE rather than contention to relieve.
#
# CB_NAMES splits them 12 ways -- invoke / getstatic / putstatic / new / instanceof / newarray
# / getfield / putfield / aload / acmp / aastore / other -- and which kind dominates decides
# the lever entirely. `getfield` says the field fast paths are missing; `invoke` says dispatch
# admission; `new` says allocation.
#
# TIMINGS FROM THIS RUN ARE NOT A MEASUREMENT (armed per-dispatch atomics). Read the counts.
param([int]$Threads=0,[int]$TimeoutS=600)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
Remove-Item "env:RAVA_FORK_STATS" -EA SilentlyContinue
Remove-Item "env:RAVA_LANE_OUTER" -EA SilentlyContinue
Remove-Item "env:RAVA_EXEC_THREADS" -EA SilentlyContinue
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_CALLBACK_STATS='1';RAVA_LANE_PROBE='5000'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
if($Threads -gt 0){ $env:RAVA_EXEC_THREADS = "$Threads" }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\cbkinds.log"
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
Write-Host ("(instrumented, {0}s wall -- COUNTS only, not a timing)" -f [math]::Round($sw.Elapsed.TotalSeconds,1))
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'callback|cb_kind|invoke=|getfield=' | Select-Object -Last 12
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'GILFREE-RINGS. batch' | Select-Object -Last 2

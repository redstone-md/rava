# WHICH COMPILED METHOD IS THE RING'S WORK, AND WHAT IS ITS MACHINE CODE?
#
# Both halves of the goal now reduce to one root: we execute ~6.6x HotSpot's machine work
# (137.6s cpu vs 20.8s, bench/wall_budget.ps1). Parallelism cannot fix it -- 68% of wall is
# serial, and on the ring side even 6 physical cores with the hyperthread term removed cap the
# speedup near 3.3x against the ~2.4x still needed (bench/lane_cpu_curve.ps1). So the ring's
# WORK has to come down, and the recorded root cause is instruction bloat in compiled code:
# ~209 machcode instructions per node-visit against the JVM's ~20.
#
# That claim is now cheaply checkable. RingScan is a pure numeric worldgen workload that runs in
# seconds, RAVA_PROFILE_PC gives TRUE self-time per compiled method, and RAVA_JIT_DUMP=<substr>
# writes the CLIF and the final machine code for matching classes to clif.txt. This finds the
# top method; dump it and the 209-vs-20 gap becomes a list of instructions to account for.
param([int]$N=8,[string]$Dump='')
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
# release-diag: `release` MIS-NAMES symbols (strip="symbols"), which has produced wrong method
# names here before.
#
# ENFORCED, NOT -WarnOnly. This script was originally written with -WarnOnly and that single
# choice produced a wrong finding: `cargo build --release` does NOT rebuild release-diag, so the
# dumps ran against a binary from an earlier session. It reported 17 explicit slots / 8.7 KB of
# frame for ShiftedNoise.compute; the same method on a current binary has THREE slots and ~1.5 KB,
# and a whole call-buffer-sharing change was built against frame bloat that does not exist. The
# instruction counts were unaffected (identical line/block/movq totals), which is exactly why the
# artifact was believable. A dump is a measurement and gets the same stale-binary guard as one.
$exe="$root\target\release-diag\rava.exe"
Assert-FreshExe $exe
$cp="$here\out;"+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE',
              'RAVA_PROFILE_PC_STACKS','RAVA_EXEC_THREADS','RAVA_JIT_DUMP'){
  Remove-Item "env:$k" -EA SilentlyContinue
}
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_PROFILE_PC='1';RAVA_PROFILE_WALL_EVERY_MS='4000'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
if($Dump){
  $env:RAVA_JIT_DUMP=$Dump
  Remove-Item "$root\clif.txt" -EA SilentlyContinue
  Write-Host ("dumping CLIF + machcode for classes containing '{0}' -> clif.txt" -f $Dump)
}
$log="$here\ringhot.log"
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'RingScan',"$N") -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
$p.WaitForExit(600000) | Out-Null
Write-Host ("wall={0}s" -f [math]::Round($sw.Elapsed.TotalSeconds,1))
Get-Content $log -EA SilentlyContinue | Select-String 'RINGS_MS|HASH'
Write-Host ""
Write-Host "=== compiled methods by TRUE self-time (last block) ==="
$lines = Get-Content "$log.err" -EA SilentlyContinue
$startIdx = -1
for($i=$lines.Count-1; $i -ge 0; $i--){
  if($lines[$i] -match 'PC-PROFILE.*(TOTAL|BLOCK)'){ $startIdx=$i; break }
}
if($startIdx -ge 0){
  for($j=$startIdx; $j -lt [Math]::Min($startIdx+16,$lines.Count); $j++){ Write-Host $lines[$j] }
}
if($Dump -and (Test-Path "$root\clif.txt")){
  $sz=(Get-Item "$root\clif.txt").Length
  Write-Host ""
  Write-Host ("clif.txt = {0:N0} bytes" -f $sz)
}

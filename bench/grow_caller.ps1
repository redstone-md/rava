# Attribute the Vec that GROWS only when the lane widens.
#
# lane_contention_profile.ps1 ranked host rows by growth from 1 to 6 lane workers and the
# top row is not dispatch, it is allocation: RawVecInner::finish_grow 1.55% -> 6.76%, with
# do_reserve_and_handle, mi_theap_realloc_aligned, mi_free and four more mimalloc rows
# behind it -- ~14 points of profile that simply do not exist at width 1. A reallocating
# Vec is a Vec that was created empty and pushed into; six workers each pay the whole
# growth curve. Which Vec is the only question left, and a leaf name cannot answer it.
param([int]$Threads=6,[int]$TimeoutS=300)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release-diag\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_FALLBACK_TALLY'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_PROFILE_PC_STACKS='1';RAVA_PC_STACK_FILTER='finish_grow,do_reserve_and_handle,reserve';RAVA_EXEC_THREADS="$Threads"}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\growcaller.log"
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
$sw=[Diagnostics.Stopwatch]::StartNew()
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 500
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
Start-Sleep -Milliseconds 400
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'PC-STACK' | Select-Object -Last 40

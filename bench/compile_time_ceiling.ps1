# THE EXACT PRICE OF MOVING CODEGEN OFF THE CRITICAL PATH.
#
# bench/phase_wall_split.ps1 put <vm>.cranelift-compile at 13.9% of JIT_EXEC, roughly 4.3s of the
# 54s serial critical path -- but that is a SAMPLER estimate, and this session's whole lesson is
# that an estimate is not a price. RAVA_COMPILE_TIME measures it directly: one Instant::now per
# compile (~1140 of them on a boot, so the probe itself is free) accumulating the real wall time
# Cranelift spends inside the compile call.
#
# The number is the CEILING for two different levers, and it is worth knowing before either is
# built: a background compile thread (pay it on another core) and an AOT code cache (do not pay
# it at all on a second boot). If it comes in well under the sampler's 4.3s, neither is worth
# building at this stage of the goal.
param([int]$TimeoutS=400)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE',
              'RAVA_PROFILE_PC','RAVA_PROFILE_PC_STACKS','RAVA_PROFILE_WALL','RAVA_JIT_STATS',
              'RAVA_EXEC_THREADS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_COMPILE_TIME='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\compiletime.log"
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 250
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
$wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
Start-Sleep -Milliseconds 400
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
powercfg /setactive $orig | Out-Null
$m = Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1
$done = if($m){ $m.Matches.Groups[1].Value } else { '-' }
Write-Host ("wall={0}s  Done={1}s   (probe is ~1140 Instant::now calls, so these ARE valid timings)" -f $wall,$done)
Write-Host ""
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'COMPILE-TIME' | Select-Object -Last 6
$last = Get-Content "$log.err" -EA SilentlyContinue | Select-String 'COMPILE-TIME' | Select-Object -Last 1
if($last -and $last.Line -match '(\d+) methods compiled, ([0-9.]+)s'){
  $n=[int]$Matches[1]; $s=[double]$Matches[2]
  Write-Host ""
  Write-Host ("=> {0} compiles, {1}s cumulative = {2:N2} ms each" -f $n,$s,(1000*$s/$n))
  Write-Host ("=> that is {0:N1}% of the {1}s wall, and the CEILING for both a background" -f (100*$s/$wall),$wall)
  Write-Host "   compile thread and an AOT code cache. Note the last print lands at a multiple"
  Write-Host "   of 500 compiles, so the true total is slightly higher than this."
}

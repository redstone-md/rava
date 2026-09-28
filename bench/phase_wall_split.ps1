# WHAT IS THE 54s OF SERIAL TIME DOING?
#
# bench/wall_budget.ps1 established the arithmetic that decides the goal: A+B=34s and C=20s run
# single-threaded, so 68% of the 79.3s wall is serial and wall<20 needs those 54s at ~14s -- a
# 3.9x SINGLE-THREAD improvement that no parallelism lever can deliver. Separately, total CPU is
# 137.6s against HotSpot's 20.8s: 6.6x the machine work for byte-identical output.
#
# This asks WHERE that serial time sits, in WALL-CLOCK terms rather than event counts, because
# counting events has now mispriced three levers in a row. RAVA_PROFILE_WALL attributes sampled
# wall time to JIT_EXEC / INTERP_EXEC / VM_DISPATCH / CLASS_LOADING / NATIVE_BRIDGES / OTHER.
#
# READ JIT_EXEC WITH CARE. Its guard wraps ALL of jit_run, so it counts host callbacks made from
# inside compiled code as though they were compiled code. RIP sampling put the boot at only
# 3-16% genuinely INSIDE compiled bodies. So JIT_EXEC here means "inside jit_run, callbacks
# included" and must never be quoted as compiled-code time. The other five categories are clean.
#
# Dumps every 5s so the cumulative rows can be differenced across the phase boundaries
# (Environment: ends A+B, "Starting minecraft server version" ends C).
param([int]$TimeoutS=400,[int]$EveryMs=5000)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE',
              'RAVA_PROFILE_PC_STACKS','RAVA_EXEC_THREADS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_PROFILE_WALL='1';RAVA_PROFILE_WALL_EVERY_MS="$EveryMs"}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\phasewall.log"
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
Write-Host ("wall={0}s  (sampler armed; the SPLIT is the output, the wall is inflated)" -f $wall)
Write-Host ""
Write-Host "--- phase boundaries from the guest's own log ---"
foreach($pat in '^\[','Environment:','Starting minecraft server version','Done \('){
  $x=Select-String -Path $log -Pattern $pat -EA SilentlyContinue | Select-Object -First 1
  if($x){ Write-Host ("  {0,-42} {1}" -f $pat, $x.Line.Substring(0,[Math]::Min(60,$x.Line.Length))) }
}
Write-Host ""
Write-Host "--- cumulative wall split (difference consecutive rows to get per-phase) ---"
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'WALL|PROFILE' | Select-Object -Last 40

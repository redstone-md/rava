# HOW MUCH OF THE BOOT IS ACTUALLY INSIDE COMPILED MACHINE CODE?
#
# This is the bound that decides where optimisation effort can possibly pay, and it is NOT the
# JIT_EXEC row. The JIT_EXEC guard wraps all of `jit_run`, so it counts the host machinery reached
# FROM compiled code - dispatch, callbacks, allocation - as JIT_EXEC too. `RAVA_PROFILE_PC` reads
# the guest thread's actual instruction pointer and splits the samples honestly:
#
#   [PC-PROFILE ... BLOCK] N RIP samples: X (a%) inside compiled code, Y (b%) in the Rust host /
#                                         JDK natives / kernel
#
# Why it matters: every codegen lever - the value-model flip, the dispatch diamonds, instruction
# bloat - can only ever act on the `a%`. If `a` is small, those levers are capped at a fraction of
# a small number, which is exactly the pattern every one of them has measured (2-6%).
#
# ⚠ MUST be the `release-diag` binary. `release` is stripped and MIS-NAMES symbols, which has
# produced wrong findings in this tree before. A diag binary's WALL is not a number to quote -
# this script measures a ratio, not a speed.
param(
  [int]$TimeoutS = 900,
  [string]$Exe = 'D:\code\rava\target\release-diag\rava.exe'
)
$root='D:\code\rava'
if(-not (Test-Path $Exe)){
  Write-Host "missing $Exe - build: cargo build --profile release-diag -p rava-cli --features next-gen-gc"
  exit 1
}
$cp='out;'+((Get-Content "$root\bench\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_JIT_STATS','RAVA_COMPILE_TIME',
              'RAVA_BRIDGE_STAT','RAVA_CELL_CENSUS','RAVA_PROFILE_PC_STACKS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$e=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';
     RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';RAVA_PROFILE_WALL='1';RAVA_PROFILE_PC='1'}
foreach($k in $e.Keys){ Set-Item "env:$k" $e[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$root\bench\inside.log"
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $Exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
    -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 400
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
Write-Host ("wall=" + [math]::Round($sw.Elapsed.TotalSeconds,1) + "s  (release-diag + PC sampling - NOT a speed measurement)")
Start-Sleep -Milliseconds 1500
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
try { $p.WaitForExit(5000) | Out-Null } catch {}
$done = Select-String -Path $log -Pattern 'Done \(' | Select-Object -First 1
if($done){ Write-Host $done.Line } else { Write-Host "*** no Done line - the split below covers a partial boot" }
Write-Host "--- inside-compiled split, per block ---"
Get-Content "$log.err" -EA SilentlyContinue | Select-String -Pattern 'RIP samples since the previous dump' |
  ForEach-Object { $_.Line }
Write-Host "--- top methods by TRUE self-time (last dump) ---"
Get-Content "$log.err" -EA SilentlyContinue | Select-String -Pattern 'PC-PROFILE\]' |
  Select-Object -Last 12 | ForEach-Object { $_.Line }

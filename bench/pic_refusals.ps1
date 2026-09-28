# WHY DO 405 MILLION INVOKES FALL TO THE HOST?
#
# The ring batch's callback census (bench/cb_kinds.ps1) is not ambiguous: of ~453M
# compiled->host callbacks, invoke is 405.4M -- 89.5%. getfield is a distant second at 30.4M
# (6.7%) and nothing else clears 5M. So the boot's host-bound character is, almost entirely,
# compiled code falling out of machine code to make a CALL.
#
# That is the same root the 32->64 call-buffer widening hit from one side (wall -14%, Done
# -31%): a callee the machine-code IC will not admit falls to the Rust gauntlet forever,
# however hot it gets. This asks how much of the population is left and WHY it is refused.
#
# picprobe splits every dynamic call out of compiled code into:
#   PIC_HIT       resolved entirely in machine code, no Rust callback
#   PIC_MISS      PIC site missed -> rava_dispatch_miss
#   PLAIN_INVOKE  site emitted NO PIC at all, by kind [static, virtual, special, interface]
# and separately the jit_direct_resolve refusal reasons for the fall-through population:
#   not-compiled (tier lag) / native-served / receiver-shape / other.
#
# The refusal split is the whole point: "not compiled yet" is a tiering lever, "native-served"
# is a native-dispatch lever, "receiver shape" is a value-model lever. They are different
# projects and the counts decide which one the goal actually needs.
#
# COUNTS ONLY -- the probe arms atomics on the dispatch path, so this run's wall is fiction.
param([int]$TimeoutS=700)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_OUTER','RAVA_EXEC_THREADS','RAVA_CALLBACK_STATS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_JIT_PIC_PROBE='1';RAVA_LANE_PROBE='5000'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\picrefusals.log"
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
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'PIC|picprobe|refus|not_compiled|native_served|recv' | Select-Object -Last 20

# SIZE THE DIRECT-MAPPED TABLE FROM DATA, NOT FROM A GUESS.
#
# Increment 2 of docs/dispatch-direct-mapped-plan.md: the table is allocated and filled but
# NOTHING READS IT, so this run cannot change behaviour -- it exists to answer the three
# questions increment 3 needs before it emits a machine-code probe:
#
#   promoted_sites   how many sites actually outgrow the 4-way PIC (the census predicts ~39)
#   max_class_id     the real class-id range, which is what `cap` must cover
#   table_entries    total memory the tables cost, at 8 B per entry
#   fills            how many (site, class) verdicts the table would be answering
#
# If promoted_sites is ~39 and the memory is small, the design holds. If promotion fires on
# hundreds of sites, PROMOTE_AFTER is too low and the tables are being handed to warmup churn
# rather than to genuine megamorphism.
#
# Runs with RAVA_JIT_PIC_PROBE too, so the PIC hit/miss split that motivates the table is
# printed from the SAME boot as the table's own numbers. COUNTS only -- both probes arm
# per-dispatch atomics, so this run's wall time is fiction.
param([int]$TimeoutS=700)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_OUTER','RAVA_EXEC_THREADS','RAVA_CALLBACK_STATS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_DISPATCH_TABLE='1';RAVA_JIT_PIC_PROBE='1';RAVA_IC_VTABLE_FALLBACK='1';RAVA_LANE_PROBE='5000'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\dtcensus.log"
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
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'DISPATCH-TABLE' | Select-Object -Last 3
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'dynamic calls out of|PIC hit|PIC miss' | Select-Object -Last 3
Get-Content "$log.err" -EA SilentlyContinue | Select-String 'IC-MISS' | Select-Object -Last 9

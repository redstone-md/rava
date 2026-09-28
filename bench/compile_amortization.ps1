# DO OUR COMPILES EVER PAY FOR THEMSELVES?
#
# <vm>.cranelift-compile is 13.9% of JIT_EXEC, about 4.3s of the 54s serial critical path
# (bench/phase_wall_split.ps1). HotSpot compiles on background threads and never pays this
# serially; we compile inline, on the thread that is trying to boot.
#
# Before building a background compile thread for it, the cheaper question: how many times does
# a compiled method actually RUN afterwards? At ~1140 bodies and ~4.3s that is ~3.8ms a compile,
# and a method invoked a handful of times after being compiled never earns that back -- the
# compile is pure serial loss. RAVA_JIT_STATS already records per method:
#   seen        invocations
#   compile_ok  successful compiles
#   run         invocations AFTER compilation (compiled_runs)
#
# This is a COUNT question, which is legitimate here, but the ACTION it implies (raise the
# threshold / refuse to compile run-once bodies) must be priced on the CLOCK afterwards -- this
# turn's repeated lesson is that a share of profile is not a price.
#
# ⚠ DOES NOT COMPLETE ON THE MC BOOT, and that is the finding about the harness. RAVA_JIT_STATS
# builds a `format!("{class}.{name}{desc}")` key on EVERY invocation, which slows the boot by
# roughly an order of magnitude: a 900s cap expired before Done. RAVA_JIT_STATS_MS (added
# alongside this) fixes the separate problem that the table never DUMPED on a killed boot, but it
# cannot fix the cost of collecting it. To make this question answerable the stat key would have
# to be interned once per (class, method) instead of formatted per call.
#
# Kept because the question is still open and the harness is correct: <vm>.cranelift-compile is
# ~4.3s of the serial critical path and nobody knows how much of it goes to bodies that never
# amortize. Priced at 2-5% of wall, it was not worth paying more measurement cost for now.
param([int]$TimeoutS=900)
$root='D:\code\rava'; $here="$root\bench"
$exe="$root\target\release\rava.exe"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_JIT_PIC_PROBE',
              'RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE','RAVA_PROFILE_PC',
              'RAVA_PROFILE_PC_STACKS','RAVA_EXEC_THREADS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_JIT_STATS='1';RAVA_JIT_STATS_MS='20000'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\compamort.log"
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 500
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
Start-Sleep -Milliseconds 600
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
Write-Host ("(instrumented, {0}s wall -- the TABLE is the output)" -f [math]::Round($sw.Elapsed.TotalSeconds,1))

$rows=@()
foreach($line in (Get-Content "$log.err" -EA SilentlyContinue)){
  if($line -match '^\[JIT-STATS\] (\S+) seen=(\d+) cold=(\d+) run=(\d+) compile_ok=(\d+) compile_fail=(\d+)'){
    $rows += [pscustomobject]@{ m=$Matches[1]; seen=[int]$Matches[2]; run=[int]$Matches[4]; ok=[int]$Matches[5] }
  }
}
# The dump is periodic; keep the LAST reading per method.
$last = $rows | Group-Object m | ForEach-Object { $_.Group | Select-Object -Last 1 }
$compiled = @($last | Where-Object { $_.ok -gt 0 })
if($compiled.Count -eq 0){ Write-Host "no compiled methods parsed - check the [JIT-STATS] format"; exit 1 }
Write-Host ""
Write-Host ("compiled methods = {0}   total compiles = {1}" -f $compiled.Count, (($compiled | Measure-Object ok -Sum).Sum))
Write-Host ""
Write-Host "post-compile invocation distribution (a compile that never amortizes is serial loss):"
foreach($b in @(0,1,2,5,10,50,100,1000)){
  $n = @($compiled | Where-Object { $_.run -le $b }).Count
  Write-Host ("  run <= {0,-5} : {1,5} methods  ({2,5:N1}% of compiled)" -f $b,$n,(100.0*$n/$compiled.Count))
}
$never = @($compiled | Where-Object { $_.run -eq 0 })
Write-Host ""
Write-Host ("NEVER RAN AFTER COMPILING: {0} methods = {1:N1}% of all compiles" -f $never.Count,(100.0*$never.Count/$compiled.Count))
Write-Host ""
Write-Host "top 12 compiled methods by post-compile runs (these are the ones that DO pay):"
$compiled | Sort-Object run -Descending | Select-Object -First 12 |
  ForEach-Object { Write-Host ("  run={0,-9} seen={1,-9} {2}" -f $_.run,$_.seen,$_.m) }
$compiled | Sort-Object run | Export-Csv "$here\compile_amortization.csv" -NoTypeInformation
Write-Host ""
Write-Host ("wrote {0}\compile_amortization.csv" -f $here)

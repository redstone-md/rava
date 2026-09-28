# Generic full-MC-boot A/B for ONE env flag, interleaved to cancel drift.
# The gate for every value-model increment is the REAL boot to Done — never
# ChunkFill FILL (a broken proxy: the cached-world boot spends ~11ms in worldgen).
#   .\boot_flag_ab.ps1 -Flag RAVA_ARENA_STR_REFCELL -Seq off,on,on,off
# Writes boot_flag_ab_<FLAG>.csv; only kills its own rava.exe pid.
#
# CORES (i5-14400F = 6P logical 0-11 + 4E logical 12-15). Rava-vs-Rava A/B defaults to
# the E-cores (0xF000): the absolute Done is 1.5-2x slower there, but the DELTA is what
# an A/B needs, and leaving the P-cores to the desktop removes the contention that made
# one OFF arm span 190.6 / 163.5 / 126.8 s. Use -Cores pcore only for honest vs-JVM
# absolutes (never quote E-core Rava against a multi-threaded JVM).
param(
  [Parameter(Mandatory=$true)][string]$Flag,
  # ACCEPTS BOTH `-Seq 'off,on,on'` and `-Seq off,on,on`. PowerShell binds the UNQUOTED form as an
  # ARRAY, and a [string] parameter then throws "Cannot convert value to type System.String" - which
  # has now cost two launches in this tree. The sibling failure mode is worse and silent: a [int]
  # parameter binds `-Values 3,4,5,6` as the single integer 3456 and the sweep runs at that value.
  # Take it loosely and normalise, so neither form can misfire.
  $Seq = 'off,on,on,off,off,on',
  [string]$Extra = '',           # extra env, "K=V;K=V", applied to BOTH arms
  [ValidateSet('ecore','pcore')][string]$Cores = 'ecore',
  # A build-profile change (fat LTO, codegen-units, PGO) cannot be toggled by an env
  # flag, so A/B it as TWO BINARIES: -ExeOff/-ExeOn point at the two builds and -Flag
  # is then irrelevant (pass a dummy). Defaults keep the single-binary env-flag mode.
  [string]$ExeOff = '',
  [string]$ExeOn = '',
  # Measure even though another rava.exe is running (the result will be contended).
  [switch]$Force,
  # Known-clean wall for THIS workload, used by the per-run contention guard below. 79.3s is the
  # current measured clean boot (bench/wall_budget.ps1). Pass 0 to disable the guard.
  [double]$CleanWallS = 79.3,
  # Skip the calibration boot (see below). Use when you know the machine is quiet and want the
  # ~90s back, or when you deliberately want a contended read.
  [switch]$SkipCalibrate,
  # Optional isolated server directory.  It must contain `out` and `world_pristine`; all world
  # resets and generated logs then stay there, so a benchmark can never mirror over the user's
  # interactive server world in the repository root.
  [string]$RunRoot = ''
)
$Seq = ($Seq -join ',')   # array form -> canonical comma string
$script:contended = 0
$script:walls = @()
$mask = if($Cores -eq 'ecore'){ 0xF000 } else { 0x0FFF }
$repoRoot='D:\code\rava'
$root = if($RunRoot){ (Resolve-Path -LiteralPath $RunRoot -EA Stop).Path } else { $repoRoot }
$here="$repoRoot\bench"
$results = if($RunRoot){ Join-Path $root 'bench-results' } else { $here }
$exe="$repoRoot\target\release\rava.exe"
foreach($required in 'out','world_pristine'){
  if(-not (Test-Path (Join-Path $root $required))){
    throw "run root '$root' is missing required '$required'"
  }
}
New-Item -ItemType Directory -Force -Path $results | Out-Null
$cp=(Join-Path $root 'out')+';'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
# Fresh-process evidence must never inherit a state-resume shortcut from the shell.
foreach($k in 'RAVA_RESTORE','RAVA_CHECKPOINT_FILE','RAVA_CHECKPOINT_OPS','RAVA_STACKFUL'){
  Remove-Item "env:$k" -EA SilentlyContinue
}
if($Extra){ foreach($kv in $Extra.Split(';')){ if($kv){ $p=$kv.Split('='); Set-Item "env:$($p[0])" $p[1] } } }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null
# QUIET-MACHINE CHECK. Every A/B run on 2026-07-26 was corrupted by a second benchmark
# sharing the core mask: Bootstrap.bootStrap spanned 52.9-119.7s (2.3x) and a -1.9% result
# had to be withdrawn. A foreign rava.exe is the signature, so name it BEFORE burning ~20
# minutes of runs. -Force proceeds anyway (sometimes you want a contended read).
$foreign = @(Get-CimInstance Win32_Process -Filter "Name='rava.exe'" -EA SilentlyContinue)
if($foreign.Count -gt 0){
  Write-Host ("WARNING: {0} rava.exe already running - this A/B will be CONTENDED and its delta unreliable:" -f $foreign.Count) -ForegroundColor Yellow
  $foreign | ForEach-Object { Write-Host ("  pid {0}  {1}" -f $_.ProcessId, $_.ExecutablePath) -ForegroundColor Yellow }
  if(-not $Force){
    Write-Host "Re-run with -Force to measure anyway, or wait for the machine to go quiet." -ForegroundColor Yellow
    powercfg /setactive $orig | Out-Null
    return
  }
}
# Refuse to measure a stale binary (see assert_fresh_exe.ps1 for the three times
# this failed silently and produced a plausible wrong number).
. "$here\assert_fresh_exe.ps1"
# Check the exes this run will ACTUALLY execute. In two-binary mode the default
# target\release\rava.exe is never launched, so failing on its vintage stops a valid
# measurement for a binary that takes no part in it — and the reflex fix for a guard that
# cries wolf is to disable the guard, which is exactly what it exists to prevent. The two
# named exes are checked -WarnOnly instead: a two-binary A/B is a DELIBERATE comparison of
# two vintages (that is the whole point), so "older than the tree" is expected there, and
# what still needs saying out loud is which one.
if($ExeOff -or $ExeOn){
  foreach($e in @($ExeOff, $ExeOn) | Where-Object { $_ }) { Assert-FreshExe $e -WarnOnly }
} else {
  Assert-FreshExe $exe
}

# CALIBRATION BOOT. A CPU% sample is NOT a proxy for whether this machine can measure: a reading of
# 26-40% "idle enough" still produced wall 141.4s against a clean 79.3s and Done 47.9s against
# 23.7s -- 1.8x inflated, and comfortably UNDER the 2x per-run guard below, which would have passed
# it silently. The per-run guard catches blowouts; only a calibration run catches moderate,
# steady contention, which is the more dangerous case precisely because it looks plausible.
#
# So: one boot with the flag OFF before the batch, compared against the known-clean wall. Costs ~90s
# and saves the ~20 minutes an unusable batch takes to produce a number nobody can use.
# -SkipCalibrate opts out; -CleanWallS 0 disables both this and the per-run guard.
if($CleanWallS -gt 0 -and -not $SkipCalibrate){
  $calExe = if($ExeOff){ $ExeOff } else { $exe }
  Set-Item "env:$Flag" $null
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $cl="$results\ab_$($Flag)_calib.log"
  Remove-Item $cl,"$cl.err" -EA SilentlyContinue
  $csw=[Diagnostics.Stopwatch]::StartNew()
  $cp0=Start-Process -FilePath $calExe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $cl -RedirectStandardError "$cl.err" -PassThru
  $cp0.ProcessorAffinity=[IntPtr]$mask
  while($csw.Elapsed.TotalSeconds -lt 700){ if($cp0.HasExited){break}; if((Test-Path $cl) -and (Select-String -Path $cl -Pattern 'Done \(' -Quiet)){break}; Start-Sleep -Milliseconds 500 }
  $cwall=[math]::Round($csw.Elapsed.TotalSeconds,1)
  Start-Sleep -Milliseconds 400; if(-not $cp0.HasExited){Stop-Process -Id $cp0.Id -Force -EA SilentlyContinue}
  $calBase = if($Cores -eq 'ecore'){ $CleanWallS * 2 } else { $CleanWallS }
  $ratio = [math]::Round($cwall / $calBase, 2)
  Write-Host ("CALIBRATION: wall={0}s vs known-clean {1}s for {2} => {3}x" -f $cwall,$calBase,$Cores,$ratio)
  if($ratio -gt 1.3){
    Write-Host "*** MACHINE CANNOT MEASURE RIGHT NOW (>1.3x). Not running the batch." -ForegroundColor Red
    Write-Host "*** Wait for it to go quiet, or pass -SkipCalibrate to measure anyway." -ForegroundColor Red
    powercfg /setactive $orig | Out-Null
    return
  }
}

$csv="run,cfg,done_s,reached,wall_s`n"; $i=0
foreach($cfg in ($Seq -split ',')){
  $i++
  if($cfg -eq 'on'){ Set-Item "env:$Flag" '1' } else { Set-Item "env:$Flag" $null }
  $runexe = if($cfg -eq 'on' -and $ExeOn){ $ExeOn } elseif($cfg -eq 'off' -and $ExeOff){ $ExeOff } else { $exe }
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $log="$results\fab_$($Flag)_$($i)_$cfg.log"
  Remove-Item $log,"$log.err" -EA SilentlyContinue
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $runexe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]$mask; $done=$false
  while($sw.Elapsed.TotalSeconds -lt 700){ if($p.HasExited){break}; if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){$done=$true;break}; Start-Sleep -Milliseconds 500 }
  $wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
  Start-Sleep -Milliseconds 500; if(-not $p.HasExited){Stop-Process -Id $p.Id -Force -EA SilentlyContinue}
  $m=(Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1); $dt= if($m){$m.Matches.Groups[1].Value}else{'NONE'}
  $csv += "$i,$cfg,$dt,$done,$wall`n"; $csv | Out-File "$results\boot_flag_ab_$Flag.csv" -Encoding utf8
  Write-Host ("run{0} {1} [{2}]: done={3}s reached={4} wall={5}s" -f $i,$cfg,$Cores,$dt,$done,$wall)
  # MID-BATCH CONTENTION GUARD. The quiet-machine check above runs ONCE, before the first boot, so
  # it cannot see load that ARRIVES during the batch -- and that is not hypothetical: a full A/B ran
  # with the user's own applications busy (a game, Discord, a browser) at a sustained 36-45%, and
  # produced wall 149-256s against a known-clean 79.3s with Done 49-70s against 23.7s, swinging 100s
  # between consecutive runs. It still printed a tidy "-2.82% OVERLAP" that meant nothing, because
  # varying contention does not affect both arms equally. Twenty minutes of runs, nearly reported.
  #
  # So sanity-check every run against a known-clean wall, not just the machine state at t=0. The
  # threshold is deliberately generous (2x) because it must not fire on an ordinary slow run or on
  # a legitimately slower arm -- it is here to catch the 2-3x blowout that says "these numbers are
  # about something other than the flag".
  # CleanWallS is a P-CORE number and this harness defaults to E-CORES, which the header above
  # records as 1.5-2x slower. Scale the baseline by the core mask or the guard cries wolf on every
  # ordinary ecore run -- and a guard that cries wolf gets disabled, which is the failure mode the
  # stale-exe check already documents.
  $script:walls += [double]$wall
  $cleanEff = if($Cores -eq 'ecore'){ $CleanWallS * 2 } else { $CleanWallS }
  if($CleanWallS -gt 0 -and $wall -gt $cleanEff * 2){
    Write-Host ("  *** CONTENDED: wall {0}s is over 2x the known-clean {1}s for these cores. This run is NOT a measurement." -f $wall,$cleanEff) -ForegroundColor Red
    $script:contended = $script:contended + 1
  }
}
Set-Item "env:$Flag" $null
powercfg /setactive $orig | Out-Null

# VERDICT. Report MIN as well as median: contention only ever ADDS time, so on a box that
# cannot be made fully quiet the minimum is the least-corrupted statistic. The strong form
# of a win here is "every run of one arm beats every run of the other" - anything weaker is
# a direction, not a number.
$rows = $csv -split "`n" | Select-Object -Skip 1 | Where-Object { $_ -match ',' } | ForEach-Object {
  $f = $_.Split(','); [pscustomobject]@{ cfg=$f[1]; done=$(if($f[2] -eq 'NONE'){$null}else{[double]$f[2]}) }
}
foreach($arm in 'off','on'){
  $v = @($rows | Where-Object { $_.cfg -eq $arm -and $null -ne $_.done } | ForEach-Object { $_.done } | Sort-Object)
  if($v.Count){
    Write-Host ("{0}: n={1} min={2}s median={3}s max={4}s  runs={5}" -f `
      $arm,$v.Count,$v[0],$v[[int]([math]::Floor($v.Count/2))],$v[-1],($v -join '/'))
  }
}
# ⚠ THE FIXED THRESHOLD ABOVE IS NOT ENOUGH, and this batch is why. On 2026-08-13 a
# RAVA_LAMBDA_PIC A/B came back with three of eight runs ~42% slow (wall 165s against a 140s norm)
# because desktop applications took cores mid-batch. The per-run guard fires at 2x the known-clean
# wall — 317s for these cores — so a 42% blowout sails through it, and a 42% blowout destroys a
# percent-level A/B exactly as thoroughly as a 3x one.
#
# So judge each run against THE BATCH, which needs no constant and no core-mask scaling: any run
# more than 25% above the fastest run of the batch saw a different machine than the fastest one did.
if($script:walls.Count -ge 3){
  $wmin = ($script:walls | Measure-Object -Minimum).Minimum
  $slow = @($script:walls | Where-Object { $_ -gt $wmin * 1.25 })
  if($slow.Count){
    Write-Host ("*** BATCH NOT LEVEL: {0} of {1} runs are >25% above the fastest ({2}s): {3}" -f `
      $slow.Count, $script:walls.Count, [math]::Round($wmin,1), (($slow | ForEach-Object { [math]::Round($_,1) }) -join ', ')) -ForegroundColor Red
    Write-Host "*** The box changed during the batch. Whatever the arms differ by below, it is not only the flag." -ForegroundColor Red
  }
}
$offv = @($rows | Where-Object { $_.cfg -eq 'off' -and $null -ne $_.done } | ForEach-Object { $_.done })
$onv  = @($rows | Where-Object { $_.cfg -eq 'on'  -and $null -ne $_.done } | ForEach-Object { $_.done })
if($offv.Count -and $onv.Count){
  $sep = if(($onv | Measure-Object -Maximum).Maximum -lt ($offv | Measure-Object -Minimum).Minimum){ 'ON wins CLEANLY (every ON < every OFF)' }
         elseif(($offv | Measure-Object -Maximum).Maximum -lt ($onv | Measure-Object -Minimum).Minimum){ 'OFF wins CLEANLY (every OFF < every ON)' }
         else { 'OVERLAP - arms are not separated; treat as a direction, not a measurement' }
  $dmin = [math]::Round((($onv | Measure-Object -Minimum).Minimum - ($offv | Measure-Object -Minimum).Minimum) / ($offv | Measure-Object -Minimum).Minimum * 100, 2)
  Write-Host ("min-of-N delta: {0}%  =>  {1}" -f $dmin,$sep)
}
# PAIRED VIEW. The -Seq alternates arms, so consecutive off/on runs see almost the same machine and
# their DIFFERENCE cancels drift the per-arm min/median cannot. This batch is why it is here: run 3
# came back at Done 42.1s / wall 114.6s against an ~84s norm because the box turned mid-batch, and a
# min-of-N over runs like that measures WHEN a run happened more than what the flag did.
$pairs = @()
$n = [math]::Min($offv.Count, $onv.Count)
for($i=0; $i -lt $n; $i++){ $pairs += [math]::Round($onv[$i] - $offv[$i], 3) }
if($pairs.Count -ge 2){
  Write-Host ''
  Write-Host ("PAIRED (on minus off, by position; negative = ON faster): {0}" -f ($pairs -join '  '))
  $neg = @($pairs | Where-Object { $_ -lt 0 }).Count
  $srt = @($pairs | Sort-Object)
  $med = if($srt.Count % 2 -eq 1){ $srt[[math]::Floor($srt.Count/2)] } else { ($srt[$srt.Count/2-1]+$srt[$srt.Count/2])/2 }
  Write-Host ("  median {0}s over {1} pairs; {2}/{1} favour ON" -f $med,$pairs.Count,$neg)
  if($neg -eq $pairs.Count -and $pairs.Count -ge 3){ Write-Host '  => every pair favours ON' }
  elseif($neg -eq 0 -and $pairs.Count -ge 3){ Write-Host '  => every pair favours OFF' }
  else { Write-Host '  => signs are MIXED - no paired verdict' }
}
if($script:contended -gt 0){
  Write-Host ""
  Write-Host ("*** {0} of {1} runs were CONTENDED (wall > 2x the known-clean baseline for {2})." -f $script:contended,$rows.Count,$Cores) -ForegroundColor Red
  Write-Host "*** TREAT THE DELTA ABOVE AS VOID. Re-run on a quiet machine." -ForegroundColor Red
}
"FLAG AB DONE" | Out-File "$results\boot_flag_ab_$Flag.done" -Encoding utf8

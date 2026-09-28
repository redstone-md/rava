# Interleaved real-boot A/B that reports the WALL **and every phase**, from one set of runs.
#
# WHY THIS EXISTS. Three rounds wanted a block-C measurement and none got one, because the two
# existing harnesses each see a single window: boot_blockb_ab.ps1 stops at `Environment:` (blocks
# A+B) and boot_flag_ab.ps1 reports only `Done` and total wall. So a lever living in block C was
# twice judged on the A+B window, and a verdict of "neutral" was recorded for a change whose own
# evidence cited a cost inclusive of block C. One harness that splits every run removes that whole
# class of mistake — and it costs nothing extra, because the phases are already in the guest log.
#
# HOW THE SPLIT IS DERIVED. The guest's own log lines, not a stopwatch:
#   process start ─┬─ first log line ····································· (launch, excluded)
#                  ├─ `Environment:`                  → end of blocks A+B
#                  ├─ `Starting minecraft server ver`  → end of block C
#                  └─ `Done (Xs)`                      → end of the Done window
# Log stamps have ONE-SECOND resolution, so each phase carries up to ±1s of quantisation — about
# ±4% on a 26s block C. That averages out across runs and is why the per-phase columns want more
# pairs than the wall column does. `Done (Xs)` is Minecraft's own figure and is exact; the harness
# prints it alongside the derived window as a cross-check (they should agree within the rounding).
#
#   .\boot_phase_ab.ps1 -Flag RAVA_DISPATCH_GATE -Seq off,on,on,off,off,on
#   .\boot_phase_ab.ps1 -Flag DUMMY -ExeOff <old> -ExeOn <new>        # two-binary vintage A/B
param(
  # Accepts BOTH `-Flag A,B` (PowerShell binds that as an array) and `-Flag 'A,B'` (one string).
  # Taking [string] only made the unquoted form - the natural one to type - a binding error.
  [Parameter(Mandatory=$true)][string[]]$Flag,
  [string]$Seq = 'off,on,on,off,off,on',
  [string]$Extra = '',
  [ValidateSet('ecore','pcore')][string]$Cores = 'pcore',
  [string]$ExeOff = '',
  [string]$ExeOn = '',
  [switch]$Force,
  [int]$TimeoutS = 700
)
$flags = @($Flag | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$mask = if($Cores -eq 'ecore'){ 0xF000 } else { 0x0FFF }
$root='D:\code\rava'; $here="$root\bench"; $exe="$root\target\release\rava.exe"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
if($Extra){ foreach($kv in $Extra.Split(';')){ if($kv){ $p=$kv.Split('='); Set-Item "env:$($p[0])" $p[1] } } }

$foreign = @(Get-CimInstance Win32_Process -Filter "Name='rava.exe'" -EA SilentlyContinue)
if($foreign.Count -gt 0 -and -not $Force){
  Write-Host ("WARNING: {0} rava.exe already running - a contended A/B's delta is unreliable. Re-run with -Force to measure anyway." -f $foreign.Count) -ForegroundColor Yellow
  $foreign | ForEach-Object { Write-Host ("  pid {0}  {1}" -f $_.ProcessId, $_.ExecutablePath) -ForegroundColor Yellow }
  return
}
. "$here\assert_fresh_exe.ps1"
foreach($e in @($exe, $ExeOff, $ExeOn) | Where-Object { $_ }) { Assert-FreshExe $e }

$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

# Seconds between two `[HH:mm:ss]` stamps, tolerating a midnight roll.
function Get-Span($a, $b) {
  if(-not $a -or -not $b){ return $null }
  $d = ([datetime]$b - [datetime]$a).TotalSeconds
  if($d -lt 0){ $d += 86400 }
  [math]::Round($d,0)
}
function Get-Stamp($log, $pattern) {
  $m = Select-String -Path $log -Pattern $pattern -EA SilentlyContinue | Select-Object -First 1
  if(-not $m){ return $null }
  if($m.Line -match '^\[(\d\d:\d\d:\d\d)\]'){ return $Matches[1] }
  $null
}

$rows=@(); $i=0
foreach($cfg in ($Seq -split ',')){
  $i++
  foreach($f in $flags){ if($cfg -eq 'on'){ Set-Item "env:$f" '1' } else { Set-Item "env:$f" $null } }
  $runexe = if($cfg -eq 'on' -and $ExeOn){ $ExeOn } elseif($cfg -eq 'off' -and $ExeOff){ $ExeOff } else { $exe }
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $tag=$flags[0]
  $log="$here\phase_$($tag)_$($i)_$cfg.log"
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $runexe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]$mask
  while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
    if($p.HasExited){break}
    if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
    Start-Sleep -Milliseconds 500
  }
  $wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
  Start-Sleep -Milliseconds 500; if(-not $p.HasExited){Stop-Process -Id $p.Id -Force -EA SilentlyContinue}

  $t0 = Get-Stamp $log '^\['                       # first guest log line
  $tE = Get-Stamp $log 'Environment:'
  $tS = Get-Stamp $log 'Starting minecraft server version'
  $tD = Get-Stamp $log 'Done \('
  $m  = Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1
  $rows += [pscustomobject]@{
    run=$i; cfg=$cfg; wall=$wall
    ab   = Get-Span $t0 $tE
    c    = Get-Span $tE $tS
    done = if($m){ [double]$m.Matches.Groups[1].Value } else { $null }
    dwin = Get-Span $tS $tD
  }
  Write-Host ("run{0} {1}: wall={2}s | A+B={3}s | C={4}s | Done={5}s" -f $i,$cfg,$wall,$rows[-1].ab,$rows[-1].c,$rows[-1].done)
  $rows | Export-Csv "$here\boot_phase_ab_$tag.csv" -NoTypeInformation -Encoding utf8
}
foreach($f in $flags){ Set-Item "env:$f" $null }
powercfg /setactive $orig | Out-Null

# Per-metric verdict. MIN is the headline: contention only ever ADDS time, so on a box that cannot
# be made quiet the minimum is the least-corrupted statistic. Separation ("every ON below every
# OFF") is what this project requires before a default flips; anything weaker is a direction.
Write-Host ""
foreach($metric in 'wall','ab','c','done'){
  $off = @($rows | Where-Object { $_.cfg -eq 'off' -and $null -ne $_.$metric } | ForEach-Object { [double]$_.$metric })
  $on  = @($rows | Where-Object { $_.cfg -eq 'on'  -and $null -ne $_.$metric } | ForEach-Object { [double]$_.$metric })
  if(-not $off.Count -or -not $on.Count){ continue }
  $offMin=($off|Measure-Object -Minimum).Minimum; $onMin=($on|Measure-Object -Minimum).Minimum
  $d=[math]::Round(($onMin-$offMin)/$offMin*100,2)
  $sep = if(($on|Measure-Object -Maximum).Maximum -lt $offMin){ 'ON CLEAN' }
         elseif(($off|Measure-Object -Maximum).Maximum -lt $onMin){ 'OFF CLEAN' }
         else { 'overlap' }
  Write-Host ("{0,-5} off min={1} [{2}]  on min={3} [{4}]  =>  {5}%  {6}" -f `
    $metric,$offMin,($off -join '/'),$onMin,($on -join '/'),$d,$sep)
}
Write-Host ""
Write-Host "A+B and C carry +-1s of log-stamp quantisation each; wall and Done do not." -ForegroundColor DarkGray

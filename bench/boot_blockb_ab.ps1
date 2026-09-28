# Interleaved real-boot A/B for a change that lands in BLOCK B (Bootstrap.bootStrap —
# block/item/entity registration).
#
# WHY NOT `Done (Xs)`. Minecraft's own `Done` timer starts at `Starting minecraft server
# version`, so the whole of block A (startup), B (bootStrap) and C (datapack/registry load)
# happens BEFORE it — a Block-B change is invisible to `Done` by construction.
# `boot_flag_ab.ps1` is the right harness for a change inside the Done window; this is the
# right one for anything before it.
#
# WHY `Environment:` AND NOT `Starting minecraft server version`. `Environment:` is the first
# line the server logs after `Bootstrap.bootStrap()` returns, so wall-to-that-line is blocks
# A+B and is dominated by Block B. Everything after it — datapack load, and in particular a
# ~30 s network timeout on the yggdrasil key fetch — is block C, which this change does not
# touch and which would otherwise dilute the effect and add most of the variance.
#
# The run is KILLED at the line, so it never reaches the world or the port — several of these
# can be in flight without fighting over either. (It also never has to bind 25599, which a
# concurrently-benchmarking sibling may hold.)
#
# -Flag takes a COMMA-SEPARATED list: a change that spans two gates is A/B'd as one arm.
# Reports every run plus min-of-N; on a contended box the minimum is the least-corrupted
# statistic, because contention can only ever add time.
#
#   .\boot_blockb_ab.ps1 -Flag RAVA_FAST_STRHASH,RAVA_FIELDKEY_SCRATCH -Seq off,on,on,off,off,on
param(
  [Parameter(Mandatory=$true)][string]$Flag,
  [string]$Seq = 'off,on,on,off,off,on',
  [ValidateSet('ecore','pcore')][string]$Cores = 'pcore',
  [string]$Exe = '',
  # Two-binary mode (same convention as boot_flag_ab.ps1): a change that lives in the
  # BINARY (new commits, build profile) is A/B'd as -ExeOff baseline / -ExeOn candidate.
  # -Flag still toggles env on the 'on' arm, so a default-OFF flag in the candidate build
  # is forced ON there; pass a dummy flag if none applies.
  [string]$ExeOff = '',
  [string]$ExeOn = '',
  # The log line that ends the measured window. Default = end of Bootstrap.bootStrap.
  [string]$Marker = 'Environment:',
  [int]$TimeoutS = 400
)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = 'D:\code\rava'                      # jar / libraries / assets live here
$mask = if($Cores -eq 'ecore'){ 0xF000 } else { 0x0FFF }
if (-not $Exe) { $Exe = Join-Path $here '..\target\release\rava.exe' }
$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$flags = @($Flag -split ',' | Where-Object { $_ })

foreach($k in @('RAVA_JIT','RAVA_JIT_TIER0','RAVA_GSON_NATIVE','RAVA_M3_BITSTORAGE')){ Set-Item "env:$k" '1' }
$env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

$off=@(); $on=@(); $i=0
# Refuse to measure a stale binary (see assert_fresh_exe.ps1).
. "$here\assert_fresh_exe.ps1"
foreach($e in @($exe, $ExeOff, $ExeOn) | Where-Object { $_ }) { Assert-FreshExe $e }

foreach($cfg in ($Seq -split ',')){
  $i++
  foreach($f in $flags){ if($cfg -eq 'on'){ Set-Item "env:$f" '1' } else { Set-Item "env:$f" $null } }
  $runexe = if($cfg -eq 'on' -and $ExeOn){ $ExeOn } elseif($cfg -eq 'off' -and $ExeOff){ $ExeOff } else { $Exe }
  $log = Join-Path $here "blockb_$($i)_$cfg.log"
  if(Test-Path $log){ Remove-Item $log -Force }
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $runexe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
      -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  try { $p.ProcessorAffinity=[IntPtr]$mask } catch {}
  $hit=$false
  while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
    $exited = $p.HasExited
    if((Test-Path $log) -and (Select-String -Path $log -Pattern $Marker -Quiet)){ $hit=$true; break }
    # Re-tested AFTER the log check: a run that dies just past the marker (e.g. the port is
    # held by a sibling benchmark) has still produced a valid measurement, and breaking on
    # HasExited first would silently discard it.
    if($exited){ break }
    Start-Sleep -Milliseconds 250
  }
  $t=[math]::Round($sw.Elapsed.TotalSeconds,1)
  if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  if($hit){ if($cfg -eq 'on'){ $on += $t } else { $off += $t } }
  Write-Host ("run{0} {1}: {2}s reached={3}" -f $i,$cfg,$t,$hit)
}
foreach($f in $flags){ Set-Item "env:$f" $null }
powercfg /setactive $orig | Out-Null

function Med($a){ $s=@($a | Sort-Object); return $s[[int]([math]::Floor($s.Count/2))] }
if($off.Count -and $on.Count){
  Write-Host ("off runs: {0}" -f ($off -join ', '))
  Write-Host ("on  runs: {0}" -f ($on  -join ', '))
  $mo=Med $off; $mn=Med $on
  $lo=($off | Measure-Object -Minimum).Minimum; $ln=($on | Measure-Object -Minimum).Minimum
  Write-Host ("== {0}: MIN off={1}s on={2}s delta={3:N1}%  |  MEDIAN off={4}s on={5}s delta={6:N1}% ==" -f `
    $Flag, $lo, $ln, (100.0*($ln-$lo)/$lo), $mo, $mn, (100.0*($mn-$mo)/$mo))
}

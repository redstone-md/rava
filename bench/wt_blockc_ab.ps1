# Interleaved A/B of the block-C step (WorldLoader.load) from bench/BootPhaseC.
# Alternates A (defaults) and B (flags) so drift/thermal effects hit both arms equally.
#   .\wt_blockc_ab.ps1 -Pairs 3 -BEnvs @{RAVA_NO_STR_CHARAT='1'}
# -BFirst runs B before A in each pair; run both orders, because within a pair the
# second run inherits the first's thermal state and that bias is worth more than the
# effect being measured on this box.
param([int]$Pairs = 3, [hashtable]$BEnvs = @{}, [string]$Tag = 'ab', [switch]$BFirst)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe  = Join-Path $here '..\target\release\rava.exe'
$cp   = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$base = @{ RAVA_JIT='1'; RAVA_PROC_CAP_MB='4096'; RAVA_HARD_MB='3584';
           RAVA_GSON_NATIVE='1'; RAVA_M3_BITSTORAGE='1'; RAVA_JIT_TIER0='1' }
$knobs = @('RAVA_BRIDGE_STAT','RAVA_NO_STR_CHARAT','RAVA_NO_TRIVIAL_FASTPATH')

$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null

function Run-One([string]$arm, [int]$i, [hashtable]$extra) {
  foreach ($k in $knobs) { Set-Item "env:$k" $null }
  foreach ($k in $base.Keys) { Set-Item "env:$k" $base[$k] }
  foreach ($k in $extra.Keys) { Set-Item "env:$k" $extra[$k] }
  $log = Join-Path $here "ab_$($Tag)_$($arm)$i.log"
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'BootPhaseC') `
        -WorkingDirectory $here -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $p.WaitForExit(1800000) | Out-Null
  $m = Select-String $log -Pattern 'STEP WorldLoader.load.*ms=(\d+)' | Select-Object -First 1
  $boot = Select-String $log -Pattern 'STEP Bootstrap.bootStrap ms=(\d+)' | Select-Object -First 1
  $ok = (Select-String $log -Pattern 'Loaded 1470 recipes') -ne $null
  $v = if ($m) { [int]$m.Matches.Groups[1].Value } else { -1 }
  $bv = if ($boot) { [int]$boot.Matches.Groups[1].Value } else { -1 }
  Write-Host ("{0}{1}: blockC={2}ms bootStrap={3}ms recipes1470={4}" -f $arm, $i, $v, $bv, $ok)
  return $v
}

$A = @(); $B = @()
foreach ($i in 1..$Pairs) {
  if ($BFirst) {
    $B += Run-One 'B' $i $BEnvs
    $A += Run-One 'A' $i @{}
  } else {
    $A += Run-One 'A' $i @{}
    $B += Run-One 'B' $i $BEnvs
  }
}
powercfg /setactive $orig | Out-Null
$ma = ($A | Measure-Object -Average -Minimum).Average; $mb = ($B | Measure-Object -Average -Minimum).Average
Write-Host ("A(default) blockC: {0} avg={1:N0}ms min={2}ms" -f ($A -join ','), $ma, ($A | Measure-Object -Minimum).Minimum)
Write-Host ("B(flags)   blockC: {0} avg={1:N0}ms min={2}ms" -f ($B -join ','), $mb, ($B | Measure-Object -Minimum).Minimum)
Write-Host ("delta A-B = {0:N0}ms ({1:N1}%)" -f ($ma - $mb), (100 * ($ma - $mb) / $mb))

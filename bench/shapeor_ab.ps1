# Interleaved ShapeOr: Rava vs the Adoptium JDK, P-core affinity + High-Performance plan
# (the two noise sources bench_run.ps1 documents: E-core hopping and frequency scaling).
# Alternates jvm/rava so any drift hits both sides equally; reports medians and the ratio.
#   .\shapeor_ab.ps1 -Runs 3 -Passes 200 -Warmup 50
param(
  [int]$Runs = 3,
  [int]$Passes = 200,
  [int]$Warmup = 50,
  [string]$Exe = ''
)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$mask = 0x0FFF                      # P-cores on the i5-14400F
$java = 'C:\Program Files\Eclipse Adoptium\jdk-25.0.3.9-hotspot\bin\java.exe'
if (-not $Exe) { $Exe = Join-Path $here '..\target\release\rava.exe' }
$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())

$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null

$env:RAVA_JIT='1'; $env:RAVA_JIT_TIER0='1'; $env:RAVA_GSON_NATIVE='1'
$env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'

# NOTE: the argv parameter must NOT be called $args — that is a PowerShell automatic
# variable inside a function, so the caller's value is silently discarded (Start-Process
# then fails on a null ArgumentList and every run reports 0 ms).
function Run-One($tag, $exe, $argv) {
  $log = Join-Path $here "shapeor_$tag.log"
  $p = Start-Process -FilePath $exe -ArgumentList $argv -WorkingDirectory $here `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  try { $p.ProcessorAffinity = [IntPtr]$mask } catch {}
  $p.WaitForExit(1800000) | Out-Null
  $ns  = (Select-String $log -Pattern 'HOTNS=(\d+)' | Select-Object -First 1).Matches.Groups[1].Value
  $sum = (Select-String $log -Pattern 'SUM=([0-9a-f]+)' | Select-Object -First 1).Matches.Groups[1].Value
  return @{ ns = [double]$ns; sum = $sum }
}

$jvm = @(); $rava = @(); $sums = @{}
foreach ($i in 1..$Runs) {
  $j = Run-One "jvm_$i"  $java @('-cp', $cp, 'ShapeOr', "$Passes", "$Warmup")
  $r = Run-One "rava_$i" $Exe  @('-cp', $cp, 'ShapeOr', "$Passes", "$Warmup")
  $jvm += $j.ns / 1e6; $rava += $r.ns / 1e6
  $sums["jvm$i"] = $j.sum; $sums["rava$i"] = $r.sum
  Write-Host ("run{0}: jvm={1:N1}ms rava={2:N1}ms ratio={3:N1}x  sum(jvm)={4} sum(rava)={5}" -f `
    $i, ($j.ns/1e6), ($r.ns/1e6), ($r.ns/$j.ns), $j.sum, $r.sum)
}
powercfg /setactive $orig | Out-Null

function Med($a) { $s = $a | Sort-Object; return $s[[int]([math]::Floor($s.Count/2))] }
$mj = Med $jvm; $mr = Med $rava
Write-Host ("== MEDIAN jvm={0:N1}ms rava={1:N1}ms RATIO={2:N1}x  ({3} passes, {4} warmup, {5} ors/pass-set) ==" -f `
  $mj, $mr, ($mr/$mj), $Passes, $Warmup, ($Passes*162*4))
# @() forces an array even for a single distinct value — otherwise Sort-Object -Unique
# returns a bare string and $d[0] indexes its first CHARACTER.
$d = @($sums.Values | Sort-Object -Unique)
if ($d.Count -eq 1) { Write-Host "GATE: SUM bit-exact across all runs = $($d[0])" }
else { Write-Host "GATE: SUM MISMATCH -> $($d -join ' ')" }

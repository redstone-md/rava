# PAIRED FLAG A/B ON A SELF-TIMING JAVA PROGRAM.
#
# The interpreter gate (`interp_flag_ab.ps1`) measures ChunkFill and nothing else, and row 378
# showed why that is not enough: a change can be worth −10% on one workload and exactly zero on the
# gate, because the gate does not contain the population. These benchmarks print their own
# `HOTNS=`, which is fixed work timed inside the guest — a cleaner signal than process wall.
#
# ⚠**The correctness marker is REQUIRED, not optional.** A run whose output does not contain
# `-Expect` is discarded, so a change that speeds the program up by breaking it cannot register as a
# win. Row 379 is why: an A/B with disjoint arms reported −5.48% on a build that could not complete
# two of five gates. This harness cannot see gates — run them separately; it only refuses to time a
# program that stopped agreeing with itself.
#
# ⚠**ITS OWN NOISE FLOOR, measured by accident and worth keeping**: a run where the `-Flag` did not
# exist yet compared the binary against ITSELF and read **−0.28%, arms overlapping**. Nothing below
# about a percent on these programs is a result.
#
#   bench\prog_flag_ab.ps1 -Prog ArithLoop -Flag RAVA_NO_HOT_LDC -Expect 'SUM=-218191804096003337'
#   bench\prog_flag_ab.ps1 -Prog CallBench -Flag X -Expect 'N=20000000' -Metric '_NS=([\d.]+)'
param(
  [Parameter(Mandatory = $true)][string]$Prog,
  [Parameter(Mandatory = $true)][string]$Flag,
  [Parameter(Mandatory = $true)][string]$Expect,
  # What to time. Defaults to the `HOTNS=` these benchmarks print; `CallBench` and friends report
  # per-call nanoseconds under other names, so the metric is a parameter rather than a second
  # harness. Every capture group found is SUMMED, which is what makes a three-number report
  # (STATIC/VIRTUAL/IFACE) comparable across arms.
  [string]$Metric = 'HOTNS=(\d+)',
  [int]$Pairs = 4,
  [string]$Exe = 'target\release\rava.exe'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
javac -nowarn -d out "bench\$Prog.java" 2>&1 | Out-Null
$env:RAVA_JIT = '0'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'

function One([string]$side) {
  # "on" = the feature ON = the opt-out UNSET.
  if ($side -eq 'on') { Remove-Item "env:$Flag" -EA SilentlyContinue } else { Set-Item "env:$Flag" '1' }
  $log = "bench\progab_$side.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', 'out', $Prog) -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
  # PowerShell 5.1 leaves ExitCode empty unless the handle is touched first.
  $null = $p.Handle
  $p.WaitForExit()
  $text = if (Test-Path $log) { Get-Content $log -Raw } else { '' }
  if ($null -eq $text -or -not $text.Contains($Expect)) {
    Write-Host ("  {0}: DISCARDED (output does not contain '{1}')" -f $side, $Expect)
    return $null
  }
  $ms = [regex]::Matches($text, $Metric)
  if ($ms.Count -eq 0) { Write-Host ("  {0}: DISCARDED (no match for {1})" -f $side, $Metric); return $null }
  $sum = 0.0
  foreach ($m in $ms) { $sum += [double]$m.Groups[1].Value }
  return $sum
}

$on = @(); $off = @()
for ($i = 0; $i -lt $Pairs; $i++) {
  # ABBA, so a monotone drift of the box cannot land on one arm.
  $order = if ($i % 2 -eq 0) { @('on', 'off') } else { @('off', 'on') }
  foreach ($s in $order) {
    $v = One $s
    if ($null -ne $v) { if ($s -eq 'on') { $on += $v } else { $off += $v } }
  }
}
if ($on.Count -eq 0 -or $off.Count -eq 0) { Write-Host 'no valid pairs'; exit 1 }
$om = ($on | Measure-Object -Minimum).Minimum
$fm = ($off | Measure-Object -Minimum).Minimum
Write-Host ("{0} / {1}" -f $Prog, $Flag)
Write-Host ("  ON  " + (($on  | ForEach-Object { '{0:N0}' -f ($_ / 1e6) }) -join ' / ') + ' ms')
Write-Host ("  OFF " + (($off | ForEach-Object { '{0:N0}' -f ($_ / 1e6) }) -join ' / ') + ' ms')
Write-Host ("  min-of-N {0:N2}%   {1}" -f (($om - $fm) * 100 / $fm),
  $(if ((($on | Measure-Object -Maximum).Maximum) -lt $fm) { 'every ON below every OFF (disjoint)' } else { 'arms overlap' }))

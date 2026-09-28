# TWO-BINARY A/B — for a change no flag can gate.
#
# ⚠**THIS IS THE SECOND-STRONGEST FORM IN THIS TREE'S ORDER OF TRUST**, below a same-binary flag A/B
# and above an across-commit comparison. Use it only when the change cannot be a flag — the operand
# slot size (rows 385-388) is the case it was written for: `Value` is 16 bytes or it is 32, there is
# no runtime switch.
#
# The two binaries alternate ABBA within one batch, so a drifting box cannot land on one of them —
# and it drifts a lot here: the same binary on the same program has read 4,720 ms and 8,669 ms hours
# apart (row 385). Absolutes across batches mean nothing; only the within-batch ratio does.
#
# ⚠**Every run must contain `-Expect`.** A binary that got faster by producing different output is
# not faster. Row 379: an A/B with disjoint arms reported −5.48% on a build that could not complete
# two of five gates — this harness cannot see gates, so run them separately.
#
#   bench\two_binary_ab.ps1 -A target\release\rava_16.exe -B target\release\rava_32.exe `
#       -Prog ArithLoop -Expect 'SUM=-218191804096003337'
param(
  [Parameter(Mandatory = $true)][string]$A,
  [Parameter(Mandatory = $true)][string]$B,
  [Parameter(Mandatory = $true)][string]$Prog,
  [Parameter(Mandatory = $true)][string]$Expect,
  [string]$Metric = 'HOTNS=(\d+)',
  [int]$Pairs = 4
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
javac -nowarn -d out "bench\$Prog.java" 2>&1 | Out-Null
$env:RAVA_JIT = '0'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'

function One([string]$exe, [string]$tag) {
  $log = "bench\twoab_$tag.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp', 'out', $Prog) -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
  # PowerShell 5.1 leaves ExitCode empty unless the handle is touched first.
  $null = $p.Handle
  $p.WaitForExit()
  $text = if (Test-Path $log) { Get-Content $log -Raw } else { '' }
  if ($null -eq $text -or -not $text.Contains($Expect)) {
    Write-Host ("  {0}: DISCARDED (output lacks '{1}')" -f $tag, $Expect); return $null
  }
  $ms = [regex]::Matches($text, $Metric)
  if ($ms.Count -eq 0) { Write-Host ("  {0}: DISCARDED (no metric)" -f $tag); return $null }
  $sum = 0.0
  foreach ($m in $ms) { $sum += [double]$m.Groups[1].Value }
  return $sum
}

$ra = @(); $rb = @()
for ($i = 0; $i -lt $Pairs; $i++) {
  $order = if ($i % 2 -eq 0) { @('A', 'B') } else { @('B', 'A') }
  foreach ($s in $order) {
    $v = if ($s -eq 'A') { One $A 'a' } else { One $B 'b' }
    if ($null -ne $v) { if ($s -eq 'A') { $ra += $v } else { $rb += $v } }
  }
}
if ($ra.Count -eq 0 -or $rb.Count -eq 0) { Write-Host 'no valid pairs'; exit 1 }
$am = ($ra | Measure-Object -Minimum).Minimum
$bm = ($rb | Measure-Object -Minimum).Minimum
Write-Host ("{0}" -f $Prog)
Write-Host ("  A " + (($ra | ForEach-Object { '{0:N2}' -f $_ }) -join ' / '))
Write-Host ("  B " + (($rb | ForEach-Object { '{0:N2}' -f $_ }) -join ' / '))
Write-Host ("  A vs B: {0:N2}%   {1}" -f (($am - $bm) * 100 / $bm),
  $(if ((($ra | Measure-Object -Maximum).Maximum) -lt $bm) { 'every A below every B (disjoint)' }
    elseif ($am -gt (($rb | Measure-Object -Maximum).Maximum)) { 'every A above every B (disjoint, AGAINST)' }
    else { 'arms overlap' }))

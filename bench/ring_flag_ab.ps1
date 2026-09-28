# Fast interleaved A/B on the RING WORKLOAD ONLY (RingScan, bit-exact gate
# 37539c6c375d0c3). A full boot costs ~55 s per arm; RingScan costs ~4 s, so a
# codegen or dispatch change aimed at the ring can be screened here in a minute and
# only promoted to a full-boot A/B if it survives.
#
# ABBA order, HIGH priority, affinity 0x0FFF, and every run must print the gate hash
# or it is discarded — a change that alters the hash is not a performance result.
param(
  [string]$Exe = 'target\release\rava.exe',
  [string]$AEnv = '',
  [string]$BEnv = '',
  [int]$Pairs = 3,
  [string]$Tag = 'ring'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

function Invoke-Ring {
  param([string]$Arm, [string]$Spec, [int]$Idx)
  foreach ($s in @($AEnv, $BEnv)) {
    if (-not $s) { continue }
    foreach ($kv in $s.Split(',')) { if ($kv -match '^([^=]+)=') { Remove-Item "Env:$($Matches[1])" -ErrorAction SilentlyContinue } }
  }
  $env:RAVA_JIT = '1'
  if ($Spec) { foreach ($kv in $Spec.Split(',')) { if ($kv -match '^([^=]+)=(.*)$') { Set-Item "Env:$($Matches[1])" $Matches[2] } } }
  $out = "bench\ringab_${Tag}_$Arm$Idx.log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'RingScan') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
  if (-not $p.WaitForExit(600000)) { $p.Kill(); return $null }
  $line = Get-Content $out -Tail 1
  if ($line -match 'RINGS_MS=(\d+).*HASH=([0-9a-f]+)') {
    if ($Matches[2] -ne '37539c6c375d0c3') { Write-Host "  $Arm$Idx HASH MISMATCH $($Matches[2])"; return $null }
    return [int]$Matches[1]
  }
  return $null
}

$a = @(); $b = @()
for ($i = 1; $i -le $Pairs; $i++) {
  $order = if ($i % 2 -eq 1) { @(@('A', $AEnv), @('B', $BEnv)) } else { @(@('B', $BEnv), @('A', $AEnv)) }
  foreach ($o in $order) {
    $ms = Invoke-Ring -Arm $o[0] -Spec $o[1] -Idx $i
    if ($null -ne $ms) { if ($o[0] -eq 'A') { $a += $ms } else { $b += $ms } }
    Write-Host ("{0}{1} {2}ms" -f $o[0], $i, $ms)
  }
}
foreach ($x in @(@('A', $a), @('B', $b))) {
  if ($x[1].Count) {
    $m = $x[1] | Measure-Object -Average -Minimum
    Write-Host ("{0}: n={1} min={2} avg={3:N0}" -f $x[0], $x[1].Count, $m.Minimum, $m.Average)
  }
}

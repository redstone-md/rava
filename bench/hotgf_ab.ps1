# Same-binary flag A/B of RAVA_HOT_GETFIELD on the INTERPRETER gate (ledger row 350).
#
# The gate is `ChunkFill 1 16` under `RAVA_JIT=0` with the big nursery that avoids the collector
# defect (row 339). Alternating off/on/on/off so a drift cancels within the pair-of-pairs, and the
# HASH is checked on every run — a speed A/B whose arms compute different answers is not one.
param([int]$Pairs = 3, [string]$Exe = 'target\release\rava.exe')
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '0'
$env:RAVA_NURSERY_KB = '1048576'
$want = 'f55ae4b5e17b36c1'
$off = @(); $on = @(); $bad = $false
foreach ($i in 1..$Pairs) {
  $order = if ($i % 2 -eq 1) { @('off','on') } else { @('on','off') }
  foreach ($s in $order) {
    if ($s -eq 'on') { $env:RAVA_HOT_GETFIELD = '1' } else { $env:RAVA_HOT_GETFIELD = $null }
    $o = "bench\hgab_$s.log"
    $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'ChunkFill', '1', '16') `
          -WorkingDirectory (Get-Location) -RedirectStandardOutput $o `
          -RedirectStandardError "$o.err" -PassThru -WindowStyle Hidden -Wait
    $line = Get-Content $o -Tail 1
    $ms = [int]([regex]::Match($line, 'FILL_MS=(\d+)').Groups[1].Value)
    $h = [regex]::Match($line, 'HASH=([0-9a-f]+)').Groups[1].Value
    if ($h -ne $want) { $bad = $true; Write-Host "*** HASH MISMATCH ($s): $h" }
    if ($s -eq 'on') { $on += $ms } else { $off += $ms }
    Write-Host "$s`t$ms`t$h"
  }
}
Write-Host ""
Write-Host ("off: n={0} min={1} runs={2}" -f $off.Count, ($off | Measure-Object -Minimum).Minimum, ($off -join '/'))
Write-Host ("on : n={0} min={1} runs={2}" -f $on.Count, ($on | Measure-Object -Minimum).Minimum, ($on -join '/'))
$mo = ($off | Measure-Object -Minimum).Minimum; $mn = ($on | Measure-Object -Minimum).Minimum
Write-Host ("min-of-N delta: {0:P2}  (negative = ON faster)" -f (($mn - $mo) / $mo))
$d = @(); for ($k = 0; $k -lt [Math]::Min($off.Count, $on.Count); $k++) { $d += ($on[$k] - $off[$k]) }
Write-Host ("paired (on minus off): {0}   favouring ON: {1}/{2}" -f ($d -join '  '), (($d | Where-Object { $_ -lt 0 }).Count), $d.Count)
if ($bad) { Write-Host "*** A HASH DIVERGED - the timing is void" }

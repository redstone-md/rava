# SAME-BINARY FLAG A/B ON THE INTERPRETER GATE — fixed work, both arms, one binary.
#
# WHY. The interpreter is the weak member (ledger row 337: compilers off, rava 1,415 s against
# `java -Xint` 46.19 s), and until row 338 it had no gate at all. `interp_gate.ps1` proves a change
# is bit-exact; this proves whether it costs or saves anything, using the change's own opt-out —
# a SAME-BINARY comparison, the strongest form in this tree's order of trust.
#
# ⚠**THE GATE'S ABSOLUTE FILL_MS IS NOT COMPARABLE ACROSS SESSIONS.** It has read 52,600 / 99,006 /
# 114,222 on this box for the same work. Only paired deltas taken INSIDE one alternating batch mean
# anything, which is why this script alternates rather than running one arm then the other.
#
# ⚠**RUN WITH `RAVA_FLAT_STAT` UNSET.** A census that fires an atomic per call adds ~1 B atomics to
# a 354 M-call gate and measures itself.
#
#   bench\interp_flag_ab.ps1 -Flag RAVA_NO_FLATCALL -Pairs 3
param(
  [Parameter(Mandatory = $true)][string]$Flag,
  [int]$Pairs = 3,
  [string]$Exe = 'target\release\rava.exe',
  [string]$NurseryKb = '1048576',
  [string]$Want = 'f55ae4b5e17b36c1'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
javac -nowarn -cp ((Get-Content 'bench\mc-cp.txt' -Raw).Trim()) -d out bench\ChunkFill.java 2>&1 | Out-Null

# "on" = the FEATURE on = the opt-out UNSET, so the report reads the way the change does.
function One([string]$side) {
  $env:RAVA_JIT = '0'
  $env:RAVA_NURSERY_KB = $NurseryKb
  $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  $env:RAVA_FLAT_STAT = $null
  if ($side -eq 'on') { Remove-Item "env:$Flag" -EA SilentlyContinue } else { Set-Item "env:$Flag" '1' }
  $out = "bench\iflagab_$side.log"
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'ChunkFill', '1', '16') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  # ⚠**TOUCH `.Handle` BEFORE WAITING.** Windows PowerShell 5.1 caches a `Start-Process -PassThru`
  # process object without an open handle, and `ExitCode` then reads back EMPTY after the process
  # ends — which an `-ne 0` test treats as a failure. The first version of this script discarded
  # all six runs of a batch that way, every one of which had produced the correct gate hash.
  # Dereferencing `.Handle` first makes .NET keep the handle open so the exit code survives.
  $null = $p.Handle
  $p.WaitForExit()
  $line = (Get-Content $out -Tail 1)
  $h = [regex]::Match($line, 'ChunkFill_HASH=([0-9a-f]+)').Groups[1].Value
  $ms = [regex]::Match($line, 'FILL_MS=(\d+)').Groups[1].Value
  # Name the reason. A discard that does not say whether the EXIT CODE or the HASH failed cannot
  # tell a broken harness from a broken build — which is exactly how the first batch was lost.
  if ($h -ne $Want) {
    Write-Host "  $side  *** DISCARDED: HASH=$h, wanted $Want ***"
    return $null
  }
  if ($p.ExitCode -ne 0) {
    Write-Host "  $side  *** DISCARDED: EXIT=$($p.ExitCode) (hash was correct) ***"
    return $null
  }
  Write-Host "  $side  FILL_MS=$ms  HASH=$h"
  return [double]$ms
}

# ⚠**NOT `$pairs`.** PowerShell variable names are CASE-INSENSITIVE, so `$pairs` IS the `[int]$Pairs`
# parameter — the loop counter. The first version accumulated deltas into it: `3 + (-1552)` made the
# bound negative and the batch ended after ONE pair, reporting an n=1 result as if it were n=3.
$on = @(); $off = @(); $deltas = @()
for ($i = 0; $i -lt $Pairs; $i++) {
  # ABBA: on/off then off/on. A straight on,off,on,off leaks any monotone drift of the box into
  # the sign of the delta — the positional bias this tree measured at +3.56% on a byte-identical
  # binary.
  $order = if ($i % 2 -eq 0) { @('on', 'off') } else { @('off', 'on') }
  Write-Host "pair $($i+1)/$Pairs  [$($order -join ',')]"
  $r = @{}
  foreach ($s in $order) { $r[$s] = One $s }
  if ($null -ne $r['on'] -and $null -ne $r['off']) {
    $on += $r['on']; $off += $r['off']; $deltas += ($r['on'] - $r['off'])
  }
}

if ($deltas.Count -eq 0) { Write-Host 'no valid pairs'; exit 1 }
$onMin = ($on | Measure-Object -Minimum).Minimum
$offMin = ($off | Measure-Object -Minimum).Minimum
Write-Host ''
Write-Host "on  (feature ON) : n=$($on.Count) min=$onMin  runs=$($on -join '/')"
Write-Host "off (feature OFF): n=$($off.Count) min=$offMin  runs=$($off -join '/')"
Write-Host ("min-of-N delta: {0:N2}%  (negative = the feature is FASTER)" -f (($onMin - $offMin) * 100.0 / $offMin))
$favour = @($deltas | Where-Object { $_ -lt 0 }).Count
Write-Host ("PAIRED (on minus off): {0}" -f ((@($deltas) | ForEach-Object { '{0:N0}' -f $_ }) -join '  '))
Write-Host "  $favour/$(@($deltas).Count) pairs favour the feature"
# Disjointness is the claim that survives a noisy box: it needs no averaging.
if (($on | Measure-Object -Maximum).Maximum -lt $offMin) { Write-Host '  => every ON below every OFF (disjoint)' }
elseif ($onMin -gt ($off | Measure-Object -Maximum).Maximum) { Write-Host '  => every ON above every OFF (disjoint, AGAINST)' }
else { Write-Host '  => the arms OVERLAP' }

# SAME-BINARY FLAG A/B ON A GATE WORKLOAD, MEASURED IN CPU TIME AND CYCLES.
#
# ⭐**WHY NOT THE BOOT.** The boot's floor on this box is ±8% (a null test against a nonexistent
# flag has read -7.34%, -7.82% and +7.45% on consecutive runs, two of them calling themselves
# disjoint). Anything worth 1-3% cannot be accepted or rejected there. A gate workload runs in
# seconds, so the same total time buys twenty runs instead of four, and CPU cycles are immune to
# the frequency scaling and E-core migration that dominate a wall reading.
#
# SAME BINARY, one env flag toggled — the only boot-adjacent instrument this tree trusts. Arms
# ALTERNATE (ABBA), the first pair is discarded as warm-up, and both min and MEDIAN are reported
# because a min alone hides a distribution that moved.
#
#   bench\gate_flag_cpu_ab.ps1 -Flag RAVA_ITER_TRAP -Value 3 -Const 'RAVA_DEOPT=1' -Pairs 6
param(
  [Parameter(Mandatory=$true)][string]$Flag,
  [string]$Value = '1',
  [string[]]$Const = @(),
  [string]$Prog = 'ChunkFill',
  # ONE space-separated string, and empty is legal. A [string[]] parameter cannot express "no
  # arguments" across a call boundary -- an empty array collapses to nothing and PowerShell then
  # reports the NEXT parameter's value as missing. Same lesson as -Flags in boot_counts_live.
  [string]$ProgArgs = '1 64',
  [string]$Expect = '8790f04ace726ecf',
  [int]$Pairs = 6,
  [int]$TimeoutS = 200
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$exe = Join-Path (Get-Location) 'target\release\rava.exe'
if (-not (Test-Path $exe)) { "missing $exe"; exit 3 }
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
foreach ($c in $Const) { $kv = $c.Split('=',2); Set-Item "env:$($kv[0])" $kv[1] }

# ⚠**POWERSHELL IS CASE-INSENSITIVE IN VARIABLE NAMES.** The first version of this script kept
# the arms in `$A`/`$B` and the current runs in `$a`/`$b` — the same two variables — so the
# accumulators were overwritten by scalars and every pair was thrown away. An instrument that
# silently discards its own data is worse than no instrument.
#
# Cycles were dropped for a related reason: `QueryProcessCycleTime` needs a live handle, and by
# the time the run is measured the process has exited. A number that reads zero is not a number.
$order = @()
function RunOnce([bool]$set) {
  if ($set) { Set-Item "env:$Flag" $Value } else { Remove-Item "env:$Flag" -EA SilentlyContinue }
  $out = 'bench\gateab.log'
  Remove-Item -LiteralPath $out, "$out.err" -EA SilentlyContinue
  $pa = @($ProgArgs -split '\s+' | Where-Object { $_ })
  $p = Start-Process -FilePath $exe -ArgumentList (@('-cp',$cp,$Prog)+$pa) `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out -RedirectStandardError "$out.err" `
        -PassThru -WindowStyle Hidden
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $ok = $p.WaitForExit($TimeoutS * 1000)
  $cpu = try { $p.TotalProcessorTime.TotalSeconds } catch { 0 }
  if (-not $ok) { $p.Kill(); $p.WaitForExit() }
  $txt = if (Test-Path $out) { Get-Content $out -Raw } else { '' }
  $good = $ok -and ($txt -match [regex]::Escape($Expect))
  $script:order += $(if ($set) { 'N' } else { 'F' })
  [pscustomobject]@{ Cpu = $cpu; Ok = $good }
}

$offRuns = New-Object System.Collections.ArrayList
$onRuns  = New-Object System.Collections.ArrayList
for ($i = 0; $i -lt $Pairs; $i++) {
  # ABBA within the pair, so a monotone drift cancels instead of landing on one arm.
  if ($i % 2 -eq 0) { $rOff = RunOnce $false; $rOn = RunOnce $true }
  else              { $rOn  = RunOnce $true;  $rOff = RunOnce $false }
  if (-not ($rOff.Ok -and $rOn.Ok)) { "pair {0}: GATE FAILED (off={1} on={2})" -f $i, $rOff.Ok, $rOn.Ok; continue }
  if ($i -eq 0) { "pair 0 discarded (warm-up): off={0:n2} on={1:n2}" -f $rOff.Cpu, $rOn.Cpu; continue }
  [void]$offRuns.Add($rOff.Cpu); [void]$onRuns.Add($rOn.Cpu)
  "pair {0}: off cpu={1,7:n2}s | on cpu={2,7:n2}s" -f $i, $rOff.Cpu, $rOn.Cpu
}
function Med($v) { $s = @($v | Sort-Object); if (-not $s.Count) { return 0 } $s[[int]([math]::Floor($s.Count/2))] }
if ($offRuns.Count -lt 2) { "no usable pairs"; exit 4 }
$offMin = ($offRuns | Measure-Object -Minimum).Minimum
$onMin  = ($onRuns  | Measure-Object -Minimum).Minimum
$offMed = Med $offRuns; $onMed = Med $onRuns
$disjoint = (($onRuns | Measure-Object -Maximum).Maximum -lt $offMin) -or (($offRuns | Measure-Object -Maximum).Maximum -lt $onMin)
"order: {0}   (F = flag unset, N = flag set)" -f ($order -join '')
"n={0}  OFF min={1:n2} med={2:n2}   ON({3}={4}) min={5:n2} med={6:n2}" -f $offRuns.Count,$offMin,$offMed,$Flag,$Value,$onMin,$onMed
"cpu  min delta {0,7:n2}%   median delta {1,7:n2}%   DISJOINT={2}" -f ((($onMin-$offMin)/$offMin)*100), ((($onMed-$offMed)/$offMed)*100), $disjoint

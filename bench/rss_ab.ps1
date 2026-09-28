# PEAK RSS, TWO BINARIES, SAME FIXED WORK.
#
# ⭐**The second axis for a change that time cannot resolve today.** Row 390 halved the operand slot
# (`Value` 32 → 16 bytes) and measured −2…−7% on three scenarios with overlapping arms, on a box
# whose own arm spread 38%. Memory is the other half of that trade and it is far less noisy: every
# operand stack, locals array, field box and captured-lambda vector holds `Value`s, and `value.rs`
# recorded the growth as the explicit price of `align(32)`.
#
# ⚠**Writes each result to the output file as it goes.** The first attempt at this measurement
# buffered everything in PowerShell and returned an EMPTY file after twenty minutes of work — the
# fifth instrument this session to produce nothing at the point of reporting.
#
# ⚠The hash is checked on every run: a binary that used less memory by doing different work is not
# using less memory.
#
#   bench\rss_ab.ps1 -A target\release\rava_16.exe -B target\release\rava_32.exe
param(
  [Parameter(Mandatory = $true)][string]$A,
  [Parameter(Mandatory = $true)][string]$B,
  [int]$Runs = 2,
  [string]$Want = 'f55ae4b5e17b36c1'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
javac -nowarn -cp ((Get-Content 'bench\mc-cp.txt' -Raw).Trim()) -d out bench\ChunkFill.java 2>&1 | Out-Null
$env:RAVA_JIT = '0'; $env:RAVA_NURSERY_KB = '1048576'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'

function PeakOf([string]$exe, [string]$tag) {
  $log = "bench\rssab_$tag.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'ChunkFill', '1', '16') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
        -PassThru -WindowStyle Hidden
  $null = $p.Handle
  $peak = 0
  while (-not $p.HasExited) {
    Start-Sleep -Milliseconds 300
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
  }
  $line = Get-Content $log -Tail 1
  $h = [regex]::Match($line, 'ChunkFill_HASH=([0-9a-f]+)').Groups[1].Value
  $ms = [regex]::Match($line, 'FILL_MS=(\d+)').Groups[1].Value
  if ($h -ne $Want) { Write-Host ("  {0}: DISCARDED hash={1}" -f $tag, $h); return $null }
  Write-Host ("  {0}  peak={1:N0} MB  FILL_MS={2}" -f $tag, ($peak / 1MB), $ms)
  return $peak
}

$pa = @(); $pb = @()
for ($i = 0; $i -lt $Runs; $i++) {
  # ABBA so a drifting box cannot settle on one binary.
  $order = if ($i % 2 -eq 0) { @('A', 'B') } else { @('B', 'A') }
  foreach ($s in $order) {
    $v = if ($s -eq 'A') { PeakOf $A 'A' } else { PeakOf $B 'B' }
    if ($null -ne $v) { if ($s -eq 'A') { $pa += $v } else { $pb += $v } }
  }
}
if ($pa.Count -eq 0 -or $pb.Count -eq 0) { Write-Host 'no valid pairs'; exit 1 }
$am = ($pa | Measure-Object -Minimum).Minimum
$bm = ($pb | Measure-Object -Minimum).Minimum
Write-Host ''
Write-Host ("A peak min = {0:N0} MB   runs {1}" -f ($am / 1MB), (($pa | ForEach-Object { '{0:N0}' -f ($_ / 1MB) }) -join '/'))
Write-Host ("B peak min = {0:N0} MB   runs {1}" -f ($bm / 1MB), (($pb | ForEach-Object { '{0:N0}' -f ($_ / 1MB) }) -join '/'))
Write-Host ("A vs B peak RSS: {0:N2}%" -f (($am - $bm) * 100 / $bm))

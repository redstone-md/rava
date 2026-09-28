# THE BOOT, TWO BINARIES: PEAK RSS AND `Done`, ALTERNATED.
#
# ⭐**Why RSS and not just time.** Row 391: on `ChunkFill` the halved operand slot read **−2.10% peak
# RSS with DISJOINT arms** (spreads of 2 MB and 0 MB) while TIME stayed overlapping through a 38%
# box drift. Memory is the quieter axis on this machine, so it is the one that can still resolve a
# small effect — and the boot is where the ledger's numbers are judged.
#
# ⚠**One pair is enough for memory and NOT enough for time.** The `Done`/wall columns are reported
# because they are free, not because n=1 settles them; the boot's own floor is ±4% (row 351) and
# this box has read the same fixed work 1.84× apart in one day (row 385).
#
#   bench\boot_rss_ab.ps1 -A target\release\rava_16.exe -B target\release\rava_32.exe
param(
  [Parameter(Mandatory = $true)][string]$A,
  [Parameter(Mandatory = $true)][string]$B,
  [int]$Pairs = 1,
  [int]$TimeoutS = 2700
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"

function BootOnce([string]$exe, [string]$tag) {
  & robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  $env:RAVA_JIT = '0'
  $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
  $log = "bench\bootrss_$tag.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
        -PassThru -WindowStyle Hidden
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $peak = 0
  $dl = [DateTime]::Now.AddSeconds($TimeoutS)
  while ([DateTime]::Now -lt $dl -and -not $p.HasExited) {
    Start-Sleep -Milliseconds 500
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
    if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { break }
  }
  $sw.Stop()
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  $m = Select-String -Path $log -Pattern 'Done \(([\d.]+)s\)' | Select-Object -Last 1
  $done = if ($m) { [double]$m.Matches.Groups[1].Value } else { 0.0 }
  if ($done -eq 0.0) { Write-Host ("  {0}: DISCARDED (never reached Done)" -f $tag); return $null }
  Write-Host ("  {0}  peak={1:N0} MB  Done={2:N1}s  wall={3:N1}s" -f $tag, ($peak / 1MB), $done, $sw.Elapsed.TotalSeconds)
  return [pscustomobject]@{ peak = $peak; done = $done; wall = $sw.Elapsed.TotalSeconds }
}

$ra = @(); $rb = @()
for ($i = 0; $i -lt $Pairs; $i++) {
  $order = if ($i % 2 -eq 0) { @('A', 'B') } else { @('B', 'A') }
  foreach ($s in $order) {
    $v = if ($s -eq 'A') { BootOnce $A 'A' } else { BootOnce $B 'B' }
    if ($null -ne $v) { if ($s -eq 'A') { $ra += $v } else { $rb += $v } }
  }
}
if ($ra.Count -eq 0 -or $rb.Count -eq 0) { Write-Host 'no valid pairs'; exit 1 }
$ap = ($ra.peak | Measure-Object -Minimum).Minimum
$bp = ($rb.peak | Measure-Object -Minimum).Minimum
$ad = ($ra.done | Measure-Object -Minimum).Minimum
$bd = ($rb.done | Measure-Object -Minimum).Minimum
Write-Host ''
Write-Host ("peak RSS  A {0:N0} MB   B {1:N0} MB   => {2:N2}%" -f ($ap / 1MB), ($bp / 1MB), (($ap - $bp) * 100 / $bp))
Write-Host ("Done      A {0:N1}s     B {1:N1}s     => {2:N2}%  (n=$Pairs per arm - direction only)" -f $ad, $bd, (($ad - $bd) * 100 / $bd))

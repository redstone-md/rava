# WHERE the ~2.4 GB peak RSS comes from. `tenured_cap()` is RAVA_HARD_MB/2 (default
# 4096 → a 2 GiB tenured arena) and the ring lane forces the nursery from 32 to 256
# MiB, which together account for almost exactly the observed peak — but only if
# those regions are actually TOUCHED rather than merely reserved. Sweeping the caps
# and watching peak working set answers that directly.
#
# Records BOTH peak working set (what Task Manager shows, and what Windows lets grow
# opportunistically when RAM is free) and peak private/committed bytes, which is the
# honest demand number.
param([int]$TimeoutSec = 240)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

$arms = @(
  @{ tag = 'default';   env = @{} },
  @{ tag = 'hard2048';  env = @{ RAVA_HARD_MB = '2048' } },
  @{ tag = 'hard1024';  env = @{ RAVA_HARD_MB = '1024' } },
  @{ tag = 'nursery32'; env = @{ RAVA_NURSERY_KB = '32768' } }
)

foreach ($a in $arms) {
  Remove-Item Env:RAVA_HARD_MB, Env:RAVA_NURSERY_KB, Env:RAVA_JIT_THRESHOLD -ErrorAction SilentlyContinue
  $env:RAVA_JIT = '1'
  foreach ($k in $a.env.Keys) { Set-Item -Path "Env:$k" -Value $a.env[$k] }

  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\rss_$($a.tag).log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath 'target\pgo-fat\rava.exe' `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High

  $peakWs = 0; $peakPriv = 0; $wall = $null
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    Start-Sleep -Milliseconds 250
    if (-not $p.HasExited) {
      $p.Refresh()
      if ($p.PeakWorkingSet64 -gt $peakWs) { $peakWs = $p.PeakWorkingSet64 }
      if ($p.PrivateMemorySize64 -gt $peakPriv) { $peakPriv = $p.PrivateMemorySize64 }
    }
    if (Test-Path $out) {
      if ((Get-Content $out -Tail 3 -ErrorAction SilentlyContinue) -match 'Done \(') { $wall = $sw.Elapsed.TotalSeconds; break }
    }
    if ($p.HasExited) { break }
  }
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $tail = (Get-Content $out -Tail 1 -ErrorAction SilentlyContinue)
  Write-Host ("{0,-10} wall={1,7}  peakWS={2,6:N0} MB  peakPrivate={3,6:N0} MB  | {4}" -f `
    $a.tag, $(if ($wall) { "{0:N1}s" -f $wall } else { 'FAIL' }), ($peakWs/1MB), ($peakPriv/1MB), $tail)
}
Remove-Item Env:RAVA_HARD_MB, Env:RAVA_NURSERY_KB -ErrorAction SilentlyContinue

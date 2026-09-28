# Concurrency of the ring lane, read off the per-run [t0,t1] intervals that
# GILFREE-RINGS prints for the first 16 runs of a batch. A collapsed batch and a
# healthy one both report claimed=127/127 completed=127/127 keep=true — the batch
# ledger cannot tell them apart. Overlapping intervals and distinct thread ids can.
param([string[]]$Files)
Set-Location $PSScriptRoot\..
foreach ($f in $Files) {
  if (-not (Test-Path $f)) { Write-Host "$f missing"; continue }
  $rows = @()
  foreach ($l in (Get-Content $f | Select-String 'run#(\d+) thread=ThreadId\((\d+)\).*t0=(\d+)ms t1=(\d+)ms aborted=(\w+)')) {
    $g = $l.Matches[0].Groups
    $rows += [pscustomobject]@{ Run = [int]$g[1].Value; Th = [int]$g[2].Value
                                T0 = [int]$g[3].Value; T1 = [int]$g[4].Value
                                Ab = ($g[5].Value -eq 'true') }
  }
  # Only the dense prefix (run# 0..16) is fully printed; later runs are sampled every
  # 16th and cannot show overlap. Aborted zero-length runs are excluded as noise.
  $r = $rows | Where-Object { $_.Run -le 16 -and -not $_.Ab -and ($_.T1 - $_.T0) -gt 50 }
  if ($r.Count -lt 2) { Write-Host ("{0,-34} too few real runs in prefix" -f (Split-Path $f -Leaf)); continue }
  $span = (($r.T1 | Measure-Object -Max).Maximum - ($r.T0 | Measure-Object -Min).Minimum)
  $sum = 0; foreach ($x in $r) { $sum += ($x.T1 - $x.T0) }
  $threads = ($r.Th | Sort-Object -Unique).Count
  $batch = (Get-Content $f | Select-String 'batch n=.* t=(\d+)ms' | Select-Object -Last 1)
  $t = if ($batch) { $batch.Matches[0].Groups[1].Value } else { '?' }
  Write-Host ("{0,-34} runs={1,2} threads={2} meanTask={3,5}ms span={4,6}ms CONCURRENCY={5,4:N2}  batch_t={6}ms" -f `
    (Split-Path $f -Leaf), $r.Count, $threads, [math]::Round($sum / $r.Count), $span, ($sum / $span), $t)
}

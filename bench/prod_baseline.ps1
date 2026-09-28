# NEW PRODUCT BASELINE after a source change was folded into a PGO rebuild.
#
# ⚠ THIS IS NOT AN A/B OF THE CHANGE, and must never be quoted as one. Two separately
# boot-trained PGO binaries differ by their TRAINING PROFILES as well as by their source, so a
# before/after delta here conflates the code with PGO noise. The clean measurement of the change
# is the same-profile ABBA in bench\lockcnt_ab.ps1 (ledger row 159).
#
# What this IS: the absolute wall / Done / RSS of the shipped artifact, n runs, so the ledger's
# product line is current.
param([int]$N = 3, [string]$Exe = 'target\pgo-fat\rava.exe')
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
$rows = @()
for ($i = 1; $i -le $N; $i++) {
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\prod_base_$i.log"
  Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $peak = 0
  $dl = [DateTime]::Now.AddSeconds(240)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 250
    if ($p.HasExited) { break }
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  $cpu = 0.0; try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $d = (Get-Content $out | Select-String 'Done \(([0-9.]+)s\)')
  $done = if ($d) { [double]$d.Matches[0].Groups[1].Value } else { -1 }
  $rows += [pscustomobject]@{
    run = $i; wall = [math]::Round($sw.Elapsed.TotalSeconds, 2); done = $done
    cpu = [math]::Round($cpu, 1); rssMB = [math]::Round($peak / 1MB, 0)
  }
}
$rows | Format-Table -AutoSize
"MEAN  wall={0:N2}  done={1:N2}  cpu={2:N1}  rss={3:N0}MB   n={4}" -f `
  (($rows | Measure-Object wall -Average).Average), (($rows | Measure-Object done -Average).Average), `
  (($rows | Measure-Object cpu -Average).Average), (($rows | Measure-Object rssMB -Average).Average), $rows.Count
"wall min/max {0:N2} / {1:N2}" -f (($rows | Measure-Object wall -Minimum).Minimum), (($rows | Measure-Object wall -Maximum).Maximum)

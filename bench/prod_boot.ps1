# THE PRODUCT NUMBER: N boots of the shipped PGO binary, on the restored world.
#
# Everything else here A/Bs two binaries against each other. This one just measures what the
# product does — wall to `Done (`, the guest's own Done timer, and peak working set — so a
# re-baseline is one command and always the same recipe.
#
# Port 25599 (never the user's 25565); the world is restored from world_pristine before every
# run (run1 generates / run2 loads is a real 2x trap, see rava-real-boot-verdict); P-core
# affinity; only the PID this script started is ever stopped.
param([int]$N = 3, [string]$Exe = 'target\pgo-fat\rava.exe', [int]$TimeoutSec = 300)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
. "$PSScriptRoot\require_quiet.ps1"
Assert-QuietBox -Context 'the product boot measurement'
# HIGH PERFORMANCE, like every other boot harness here. Without it the run lands on a scaled
# clock and the number is not comparable with anything in the ledger (rava-bootbench-noise-infra:
# the documented 2.1x spread on this box is E-core hop plus frequency scaling).
$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'; $env:RAVA_JIT_TIER0 = '1'
$res = @()
foreach ($i in 1..$N) {
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $log = "bench\prod_boot_$i.log"
  Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
        -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $peak = 0
  $dl = [DateTime]::Now.AddSeconds($TimeoutSec)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 200
    if ($p.HasExited) { break }
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
    if (Test-Path $log) { if ((Get-Content $log -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $d = (Get-Content $log | Select-String 'Done \(([0-9.]+)s\)')
  $done = if ($d) { [double]$d.Matches[0].Groups[1].Value } else { -1 }
  $rss = [math]::Round($peak / 1MB)
  $res += [PSCustomObject]@{ wall = $sw.Elapsed.TotalSeconds; done = $done; rss = $rss }
  "run {0}: wall={1,6:N2}s done={2,6:N2}s rss={3,5}MB" -f $i, $sw.Elapsed.TotalSeconds, $done, $rss
}
"PRODUCT n={0}: wall mean={1,6:N2} min={2,6:N2} · done mean={3,6:N2} min={4,6:N2} · peak rss={5}MB" -f `
  $res.Count, ($res | Measure-Object wall -Average).Average, ($res | Measure-Object wall -Minimum).Minimum, `
  ($res | Measure-Object done -Average).Average, ($res | Measure-Object done -Minimum).Minimum, `
  ($res | Measure-Object rss -Maximum).Maximum
if ($orig) { powercfg /setactive $orig | Out-Null }

# The CURRENT locked-callee population, with the instrument ARMED.
#
# ring-lambda-lockfree-plan.md carries its own warning that its population table was built from
# readings taken without RAVA_FORK_STATS, so the shares in it are shares of a remainder that
# turned out to be far smaller than assumed. Row 158 makes the question live again for a
# different reason: 18.3% of the ring's LIVE cpu is lock machinery, a cost wait_ms could not
# see, and the pre-lock probe template (row 127, AtomicLong#get) is the proven way to remove
# acquisitions one callee at a time.
#
# So: which callees still take the lock, ranked, measured now rather than quoted from a doc.
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_HEARTBEAT = '1'
$env:RAVA_HEARTBEAT_OPS = '100000'
$env:RAVA_FORK_STATS = '1'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\oprate.log'
Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath 'target\release-diag\rava.exe' `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds(300)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
}
$sw.Stop()
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
foreach ($n in 'RAVA_HEARTBEAT','RAVA_HEARTBEAT_OPS','RAVA_FORK_STATS','RAVA_PROC_CAP_MB','RAVA_HARD_MB') {
  [Environment]::SetEnvironmentVariable($n, $null)
}
"wall={0:N1}s" -f $sw.Elapsed.TotalSeconds
Get-Content $out | Select-String 'Done \(' | ForEach-Object { $_.Line }
"--- last LANE-PROBE line ---"
Get-Content "$out.err" | Select-String 'LANE-PROBE\] t=' | Select-Object -Last 1 |
  ForEach-Object { $_.Line }
"--- locked callees (top 12) ---"
Get-Content "$out.err" | Select-String 'locked=|LOCKED-TOP|table_hit|wsnative' | Select-Object -Last 6 |
  ForEach-Object { $_.Line }

# WHEN does the coordinator PARK? The by-function host table ranks the whole block and puts
# `ZwWaitForAlertByThreadId` first at ~20.75% — the single largest host row in the profile, 8x
# the second. The sampler follows ONE thread (pc_profile::arm_current_thread, first caller
# wins), so those samples are the COORDINATOR blocked, not idle workers.
#
# Blocked while the ring computes is a join and costs nothing extra. Blocked during the serial
# prefix is dead wall. The cumulative table cannot tell those apart; [PC-WHEN] can.
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_PROFILE_PC = '1'
$env:RAVA_PROFILE_PC_ALL = '1'
$env:RAVA_PROFILE_WALL_EVERY_MS = '8000'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\when_park.log'
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
foreach ($n in 'RAVA_PROFILE_PC','RAVA_PROFILE_PC_ALL','RAVA_PROFILE_WALL_EVERY_MS','RAVA_PROC_CAP_MB','RAVA_HARD_MB') {
  [Environment]::SetEnvironmentVariable($n, $null)
}
"wall={0:N1}s" -f $sw.Elapsed.TotalSeconds
Get-Content $out | Select-String 'Done \(|Environment:|Loaded 1470|Preparing level' |
  ForEach-Object { $_.Line }
"--- PC-WHEN ---"
Get-Content "$out.err" | Select-String '^\[PC-WHEN\]' | ForEach-Object { $_.Line }
"--- PC-ALL ---"
Get-Content "$out.err" | Select-String '^\[PC-ALL\]' | ForEach-Object { $_.Line }

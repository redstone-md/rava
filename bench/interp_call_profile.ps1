# DECOMPOSE ONE INTERPRETED CALL.
#
# Measured head to head (ledger row 185): HotSpot -Xint does this loop's call in 13.9 ns,
# rava with RAVA_JIT=0 does it in 172 ns -- 12.4x, or ~29 ns per bytecode against ~2.3 ns.
# CallBench2's body is `s = addS(s, 1)` in a loop, so the profile is the CALL and the loop,
# nothing else. JIT OFF, and the enumerating capture because nothing arms the single-target
# sampler when no compiled code runs.
Set-Location $PSScriptRoot\..
$env:RAVA_JIT = '0'
$env:RAVA_PROFILE_PC = '1'
$env:RAVA_PROFILE_PC_ALL = '1'
$env:RAVA_PROFILE_WALL_EVERY_MS = '60000'
$out = 'bench\interp_call.log'
Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath 'target\release-diag\rava.exe' `
      -ArgumentList @('-cp', 'out', 'CallBench2', '30000000') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
if (-not $p.WaitForExit(300000)) { $p.Kill(); $p.WaitForExit() }
foreach ($n in 'RAVA_JIT', 'RAVA_PROFILE_PC', 'RAVA_PROFILE_PC_ALL', 'RAVA_PROFILE_WALL_EVERY_MS') {
  [Environment]::SetEnvironmentVariable($n, $null)
}
Get-Content $out | Select-String 'RUN' | Select-Object -Last 2 | ForEach-Object { $_.Line }
Get-Content "$out.err" | Select-String 'host functions' -Context 0, 26 |
  ForEach-Object { $_.Context.PostContext } |
  ForEach-Object { $_.Substring(0, [Math]::Min(105, $_.Length)) }

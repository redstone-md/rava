# WHERE DOES rava's INTERPRETER SPEND ITS TIME?
#
# Ledger row 182 split the gap in two: rava JIT-off is >=93.8 s against HotSpot's -Xint at
# 47.89 s, so our interpreter is ~2x theirs -- a separate disease from the JIT multiplier
# (2.0x against C1's 8.3x). This isolates the first one: JIT OFF, so nothing but the
# interpreter and the native layer are in the picture.
#
# The run is killed at the window; the PREFIX profile is the deliverable, not a wall number.
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '0'
$env:RAVA_PROFILE_PC = '1'
# The single-target sampler arms on ENTRY TO COMPILED CODE, so with the JIT off it never arms
# at all (measured: zero rows). The enumerating capture has no such dependency.
$env:RAVA_PROFILE_PC_ALL = '1'
$env:RAVA_PROFILE_WALL_EVERY_MS = '30000'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\interp_prof.log'
Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath 'target\release-diag\rava.exe' `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds(150)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 400
  if ($p.HasExited) { break }
  if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
}
$sw.Stop()
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
foreach ($n in 'RAVA_JIT', 'RAVA_PROFILE_PC', 'RAVA_PROFILE_PC_ALL', 'RAVA_PROFILE_WALL_EVERY_MS') {
  [Environment]::SetEnvironmentVariable($n, $null)
}
"wall={0:N1}s (killed at the window; the prefix profile is the deliverable)" -f $sw.Elapsed.TotalSeconds
Get-Content "$out.err" | Select-String '^\[PC-ALL\]' | Select-Object -First 26 |
  ForEach-Object { $_.Line.Substring(0, [Math]::Min(110, $_.Line.Length)) }

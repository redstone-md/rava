# WHICH pattern switch fails under RAVA_ARENA_LAMBDA? `RAVA_TRACE_SWITCH` already
# prints the selector and the label list on a no-match — that is the measurement the
# ledger's retracted hypothesis should have started from.
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_ARENA_LAMBDA = '1'
$env:RAVA_TRACE_SWITCH = '1'
$env:RAVA_TRACE_FICELL = '1'
$env:RAVA_NO_INSTANCEOF_MEMO = '1'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\arenalam_probe.log'
Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath 'target\fifix.exe' `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds(200)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
}
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
Remove-Item Env:RAVA_ARENA_LAMBDA, Env:RAVA_TRACE_SWITCH, Env:RAVA_TRACE_FICELL, Env:RAVA_NO_INSTANCEOF_MEMO -ErrorAction SilentlyContinue
Get-Content "$out.err" | Select-String 'SWITCH-NOMATCH|uncaught' | Select-Object -First 6 |
  ForEach-Object { $_.Line.Substring(0, [Math]::Min(300, $_.Line.Length)) }

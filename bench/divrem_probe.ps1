# WHO calls rava_i64_divrem 8 million extra times under RAVA_ARENA_LAMBDA? The split
# census (ledger row 48) proved it is that single callback and nothing else; a census
# names the callee, never the caller, so this prints the compiled method for the first
# handful of calls.
param([string]$Exe = 'target\divp.exe')
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_ARENA_LAMBDA = '1'
$env:RAVA_TRACE_DIVREM = '1'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\divp.log'
Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds(200)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if ((Get-Content $out -Tail 2 -ErrorAction SilentlyContinue) -match 'Done \(') { break }
}
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
Remove-Item Env:RAVA_ARENA_LAMBDA, Env:RAVA_TRACE_DIVREM -ErrorAction SilentlyContinue
Get-Content "$out.err" | Select-String 'DIVREM' | Select-Object -First 8 |
  ForEach-Object { $_.Line.Substring(0, [Math]::Min(190, $_.Line.Length)) }

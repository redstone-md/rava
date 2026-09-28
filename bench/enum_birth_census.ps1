# WHICH term forces an enum-shaped class to be born as a HANDLE even with
# RAVA_ARENA_ENUM on? The cell census (ledger row 55) showed the flag moves the flip
# metric by ZERO and leaves both 30,837-cell enum targets untouched, yet the gate is
# wired at BOTH birth paths (opcodes new_object_value and its JIT twin). The tree
# already carries the instrument for exactly this question.
param([string]$Exe = 'target\pgo-fat\rava.exe', [int]$TimeoutSec = 180)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_ARENA_ENUM = '1'
$env:RAVA_ARENA_RECORD = '1'
$env:RAVA_ENUM_BIRTH_CENSUS = '1'
$env:RAVA_CELL_CENSUS = '1'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\enumcensus.log'
Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds($TimeoutSec)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if ((Get-Content $out -Tail 2 -ErrorAction SilentlyContinue) -match 'Done \(') { break }
}
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
Remove-Item Env:RAVA_ARENA_RECORD, Env:RAVA_ARENA_ENUM, Env:RAVA_ENUM_BIRTH_CENSUS, Env:RAVA_CELL_CENSUS -ErrorAction SilentlyContinue
Write-Host (Get-Content $out -Tail 1)
Get-Content "$out.err" | Select-String 'ENUM-BIRTH' | Select-Object -First 12 |
  ForEach-Object { $_.Line.Substring(0, [Math]::Min(190, $_.Line.Length)) }

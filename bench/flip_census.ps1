# BLOCKING-CELL CENSUS for one env spec, on a real server boot.
#
# The value-model arc pays only at its final flip, which is legal only when no non-0b00 ref
# cell can persist, so the arc's progress metric is the BLOCKING CELL COUNT, not wall. This
# runs one boot with `RAVA_CELL_CENSUS` armed and prints the high-water reading plus the
# ranked targets, each with the DISTINCT-OBJECT count behind its cells — the number that
# separates "30,837 cells over 4 shared singletons" from "over 30,000 per-instance objects",
# which the cell count alone cannot do and which is what root-caused the inert enum flag.
#
#   .\bench\flip_census.ps1 -Env "RAVA_ARENA_BIRTH_NS_GUARD=1" -Tag nsguard
#
# The census walks the live graph on every collection, so the wall of this run is meaningless.
param(
  [string]$Env = "",
  [string]$Tag = "base",
  [string]$Exe = 'target\release-diag\rava.exe',
  [int]$TimeoutSec = 300
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..

# Clear every name this script or the caller might set, so a previous run cannot leak into
# this one (a hardcoded list once leaked MIMALLOC_ALLOW_LARGE_OS_PAGES across seven runs).
foreach ($k in 'RAVA_ARENA_BIRTH_NS_GUARD', 'RAVA_NO_ARENA_ENUM', 'RAVA_NO_ARENA_RECORD',
                'RAVA_ARENA_LAMBDA', 'RAVA_ARENA_MAPS', 'RAVA_PINNED_ENUM',
                'RAVA_ENUM_BIRTH_CENSUS') {
  Remove-Item "Env:$k" -ErrorAction SilentlyContinue
}
$env:RAVA_JIT = '1'
$env:RAVA_CELL_CENSUS = '1'
if ($Env) {
  foreach ($pair in $Env.Split(',')) {
    if ($pair -match '^([^=]+)=(.*)$') { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
  }
}

robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = "bench\flipcensus_$Tag.log"
Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue

$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', ('out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())), 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
$dl = [DateTime]::Now.AddSeconds($TimeoutSec)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if ((Get-Content $out -Tail 2 -ErrorAction SilentlyContinue) -match 'Done \(') { break }
}
$reached = (Get-Content $out -ErrorAction SilentlyContinue | Select-String 'Done \(') -ne $null
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
Remove-Item Env:RAVA_CELL_CENSUS -ErrorAction SilentlyContinue

Write-Host ("[$Tag] reached Done: {0}" -f $reached)
Get-Content "$out.err" | Select-String 'ref cells|CELL-CENSUS. target' | Select-Object -Last 13 |
  ForEach-Object { $_.Line }

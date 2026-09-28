# WHERE do the +9.5 serial seconds of RAVA_ARENA_LAMBDA go?
#
# The flag is the single biggest lever on the flip's progress metric (-17.6% blocking
# cells, $Lambda 212,277 cells over 39,256 objects -> 179,438 over 4,055) and it also
# takes RSS down 14.4%. Its whole cost is in the SERIAL PREFIX, and the split says so
# precisely: `Done` is unchanged-to-better (19.50 -> 18.98), while block A+B goes 24 -> 28
# and block C goes 15 -> 21.
#
# A callback census cannot answer this. This tree has refuted the "count host callbacks to
# price host time" model twice (the dispatch table removed 12.6% of all PIC misses and was
# worth zero), so this samples the instruction pointer and compares SELF TIME per symbol
# between the two arms instead of counting events.
#
# release-diag + the wall-profile feature ONLY: `release` is stripped and MIS-NAMES
# symbols, which has produced wrong findings in this tree before.
param(
  [string]$Exe = 'target\release-diag\rava.exe',
  [int]$TimeoutSec = 300
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

foreach ($arm in @(@('off', $null), @('on', '1'))) {
  Remove-Item Env:RAVA_ARENA_LAMBDA -ErrorAction SilentlyContinue
  $env:RAVA_JIT = '1'
  $env:RAVA_PROFILE_WALL = '1'
  $env:RAVA_PROFILE_PC = '1'
  if ($arm[1]) { $env:RAVA_ARENA_LAMBDA = $arm[1] }
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\lamrip_$($arm[0]).log"
  Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
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
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  Write-Host ("== {0}: {1}" -f $arm[0], (Get-Content $out -Tail 1))
  Get-Content "$out.err" | Select-String 'WALL-CAT|PC-PROFILE' | Select-Object -First 26 |
    ForEach-Object { "   " + $_.Line }
}
Remove-Item Env:RAVA_PROFILE_WALL, Env:RAVA_PROFILE_PC, Env:RAVA_ARENA_LAMBDA -ErrorAction SilentlyContinue

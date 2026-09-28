# PC profile of a REAL boot, in blocks, all the way to `Done (`.
#
# `pcstack_boot.ps1` cannot do this and it is worth saying why, because the difference is what
# makes the numbers real: it arms RAVA_PC_TRACE (so `pc_profile::dump` returns after the trace and
# prints no TABLES at all) and it never sets RAVA_PROFILE_PC_ALL (so the sampler follows ONE
# thread — the first into compiled code — which is how a third of a boot once got attributed to a
# parked thread, ledger row 156). It also defaults to stopping at `Environment:`, which is the end
# of block A.
#
# This one arms the enumerating capture, dumps a ranked host table every -EveryMs, and runs to
# `Done (`. Read the LAST blocks for the `Preparing level` -> `Done (` window.
#
# Needs symbols and unfused functions. PC sampling works without wall-profile;
# that feature also enables per-call bridge registration when PC sampling is on.
#   cargo rustc --profile release-diag -p rava-cli --features next-gen-gc -- -C link-arg=/OPT:NOICF
#
#   bench\boot_pc.ps1 -EveryMs 5000 -Tag blockd
param(
  [int]$EveryMs = 5000,
  [string]$Tag = 'boot_pc',
  [string]$Marker = 'Done \(',
  [int]$TimeoutS = 600,
  # WHICH BINARY. Defaults to this tree's release-diag. Given explicitly it can
  # profile a binary built from another ref (a worktree) while still running in
  # THIS tree — which is the only way to diff two profiles over the same world,
  # classpath and flags. `world_pristine` and the restored `world` live here and
  # nowhere else, so profiling inside a worktree would measure a different
  # workload (a GENERATED world instead of a loaded one) and the two tables would
  # not be comparable at all.
  [string]$Exe = '',
  # CALLER ATTRIBUTION for one host symbol: arms `RAVA_PROFILE_PC_STACKS` with this leaf filter.
  # Off by default because a stack capture per sample is expensive — and because that default is
  # exactly why asking this harness "who calls memchr?" returned silence until this switch existed.
  [string]$Stacks = ''
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
. "$PSScriptRoot\require_quiet.ps1"
Assert-QuietBox -Context 'the boot profile' -WarnOnly
$exe = if ($Exe) { $Exe } else { 'target\release-diag\rava.exe' }
if (-not (Test-Path $exe)) {
  "missing $exe — cargo rustc --profile release-diag -p rava-cli --features next-gen-gc -- -C link-arg=/OPT:NOICF"
  exit 1
}
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
# The product's flag set (bench/prod_boot.ps1), so the profile describes what ships.
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'; $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$env:RAVA_PROFILE_PC = '1'
$env:RAVA_PROFILE_PC_ALL = '1'
$env:RAVA_PROFILE_PC_MS = "$EveryMs"
# ⚠`RAVA_PROFILE_PC_STACKS` is cleared here BY DESIGN (a stack capture per sample is expensive),
# which is also why asking this harness for caller attribution silently returned nothing until
# `-Stacks` existed. Pass `-Stacks <substr>` to arm it with a leaf filter.
$keep = @('RAVA_PC_TRACE', 'RAVA_RESTORE', 'RAVA_STACKFUL')
if (-not $Stacks) { $keep += 'RAVA_PROFILE_PC_STACKS' }
foreach ($k in $keep) {
  Remove-Item "env:$k" -ErrorAction SilentlyContinue
}
if ($Stacks) {
  $env:RAVA_PROFILE_PC_STACKS = '1'
  $env:RAVA_PC_STACK_FILTER = $Stacks
  if (-not $env:RAVA_PC_TOP) { $env:RAVA_PC_TOP = '20' }
}

$log = "bench\$Tag.log"
Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
      -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$hit = $false
while ($sw.Elapsed.TotalSeconds -lt $TimeoutS) {
  Start-Sleep -Milliseconds 250
  if ($p.HasExited) { break }
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern $Marker -Quiet)) { $hit = $true; break }
}
# Let the sampler emit one more block AFTER the marker so the last window is not half-sampled.
if ($hit) { Start-Sleep -Milliseconds ($EveryMs + 500) }
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
foreach ($k in 'RAVA_PROFILE_PC', 'RAVA_PROFILE_PC_ALL', 'RAVA_PROFILE_WALL_EVERY_MS') {
  Remove-Item "env:$k" -ErrorAction SilentlyContinue
}
"{0}: {1}s reachedDone={2} -> {3}.err" -f $Tag, [math]::Round($sw.Elapsed.TotalSeconds, 1), $hit, $log

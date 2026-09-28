# Re-price a LIST of flags on FIXED WORK, one paired gate A/B each.
#
# The boot counterpart (`flag_sweep.ps1`) cannot resolve a change confined to
# compiled code: compiled code is 19.7% of a boot, so even a 20% win there is
# ~4% of wall against a ±4% noise floor. Three separate sessions closed
# `RAVA_DISPATCH_TABLE` as "worth zero" on exactly that instrument; on fixed work
# it is −19.9% with 9 of 9 pairs. This sweep is the boot sweep's replacement for
# anything that lives inside the compiled tier.
#
# The flag is set on the AFTER arm only, with the SAME binary on both sides, so
# each row prices one feature and nothing else. Read the sign carefully: for a
# default-ON feature the natural flag is its opt-out, and then a POSITIVE delta
# means the feature is earning its keep.
#
#   bench\gate_sweep.ps1 -Exe target\release\rava.exe -Gate chunkfill -Flags RAVA_NO_DISPATCH_TABLE=1,RAVA_M3_IC_PARALLEL=1
param(
  [string]$Exe = 'target\release\rava.exe',
  [ValidateSet('chunkfill', 'ringscan', 'ringlane', 'ringlanei')][string]$Gate = 'chunkfill',
  [string[]]$Flags = @(),
  [int]$N = 7,
  [int]$Tries = 8
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
if (-not $Flags) { 'need -Flags NAME=VALUE,...'; exit 3 }
foreach ($f in $Flags) {
  "`n##### $Gate : $f"
  foreach ($i in 1..$Tries) {
    & "$PSScriptRoot\gate_ab.ps1" -BeforeExe $Exe -AfterExe $Exe -Gate $Gate -N $N -Flag $f
    $c = $LASTEXITCODE
    if ($c -eq 0) { break }
    if ($c -eq 2) { "*** $f : verdict withheld (hash mismatch or timeout)"; break }
    "retry $i (exit $c)"
    Start-Sleep -Seconds 40
  }
}
exit 0

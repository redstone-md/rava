# Re-measure a LIST of flags on CPU-SECONDS, one paired A/B each.
#
# WHY THIS EXISTS. Eight levers were called "neutral" in one session on wall
# alone, and the arithmetic afterwards showed the paired wall A/B on this box
# resolves about +-4% while the changes worth making are worth 1.6-3%. Every one
# of those verdicts was "invisible", not "zero" — and the two are different
# claims. `TotalProcessorTime` measures the work THIS process did and is not
# lengthened by whatever else the box is running, so it can separate them.
#
# The first entry should be a CONTROL with a known-large effect, so a sweep that
# reports "everything is neutral" can be told apart from a metric that cannot
# hear. `RAVA_ARENA_MAPS=1` is measured at +48% wall and serves as that control.
#
#   bench\flag_sweep.ps1 -Flags RAVA_ARENA_MAPS=1,RAVA_NO_DISPATCH_TABLE=1 -N 5
param(
  [string[]]$Flags = @(),
  [int]$N = 5,
  [string]$Exe = 'target\pgo-fat\rava.exe',
  [int]$Tries = 12
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
if (-not $Flags) { 'need -Flags NAME=VALUE,NAME=VALUE'; exit 3 }
foreach ($f in $Flags) {
  "`n===== $f ====="
  foreach ($i in 1..$Tries) {
    & "$PSScriptRoot\flag_ab.ps1" -Flag $f -N $N -Exe $Exe
    $c = $LASTEXITCODE
    if ($c -eq 0) { break }
    if ($c -eq 2) { "*** $f : an arm never reached Done"; break }
    "retry $i (exit $c)"
    Start-Sleep -Seconds 40
  }
}
exit 0

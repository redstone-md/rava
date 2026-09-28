# DOES THIS BINARY KNOW THIS FLAG? Dot-source this from any harness that takes a -Flag.
#
#   . "$PSScriptRoot\assert_flag.ps1"
#   Assert-FlagKnown -Exe $Exe -Flag 'RAVA_BULK_READ=1'
#
# WHY THIS EXISTS. On 2026-08-15 a flag A/B ran four alternating boot pairs and reported
# wall +0.07%, cpu +0.02%, done +0.90% — a clean, quiet, perfectly-paired NOTHING. The
# flag did not exist in the binary under test: `flag_ab.ps1` defaults to
# `target\pgo-fat\rava.exe`, and the code implementing the flag had only been built into
# `target\release\rava.exe`. Eight boots measured the difference between a thing and
# itself, and the output looked exactly like a real neutral verdict.
#
# That is `docs/how-to-measure.md` rule 8 — a list of flags that all report "neutral" is
# indistinguishable from an instrument that cannot hear — arriving one layer lower than
# the rule was written for: not "can the instrument resolve it" but "is the knob wired to
# anything at all". A refusal here is cheap; a false neutral is a wrong conclusion that
# gets quoted for months.
#
# The test is the flag NAME appearing literally in the executable's bytes. A flag read
# through this tree's alias table still has its name interned there, so absence is strong
# evidence the build predates the flag.

function Assert-FlagKnown {
  param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Flag,
    # Print and continue instead of refusing — for a caller that knows the name is
    # computed rather than literal.
    [switch]$WarnOnly
  )
  if (-not (Test-Path $Exe)) { "REFUSING: missing binary $Exe"; exit 3 }
  $name = $Flag.Split('=', 2)[0]
  if (-not $name) { return }
  $stamp = (Get-Item $Exe).LastWriteTime.ToString('yyyy-MM-dd HH:mm')
  # FAILS CLOSED. The first version used [Text.Encoding]::Latin1, which does not exist in
  # Windows PowerShell 5.1; the call threw, `$text` stayed null, IndexOf threw too, and the
  # function fell through to print "is present" — a guard that answered YES when it had not
  # looked. Codepage 28591 is ISO-8859-1 and does exist here; one byte per char, so an
  # ASCII needle matches the raw bytes exactly.
  $text = $null
  try {
    $text = [Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes($Exe))
  } catch {
    "REFUSING: could not read $Exe to check for '$name' — $($_.Exception.Message)"
    exit 5
  }
  if (-not $text) { "REFUSING: read $Exe as empty while checking for '$name'"; exit 5 }
  if ($text.IndexOf($name) -lt 0) {
    $msg = "REFUSING: '$name' does not appear in $Exe (built $stamp) — that build predates the flag, so both arms would run the SAME code and report a false neutral."
    if ($WarnOnly) { $msg; return }
    $msg
    exit 5
  }
  "flag check: '{0}' is present in {1} (built {2})" -f $name, $Exe, $stamp
}

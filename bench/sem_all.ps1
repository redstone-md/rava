# EVERY DIFFERENTIAL PROBE, AGAINST `java -Xint`, IN ONE RUN.
#
# The probes were built one at a time and run by hand, which meant the suite was only
# ever as complete as whatever I remembered to re-run — and rule 20 exists because a
# batch shipped with three regressions while ten probes were green and never re-run.
# This runs all of them, so "the probes pass" becomes a statement about the suite
# rather than about a sample of it.
#
# Each probe prints deterministic lines and documents its OWN expected divergences in
# its header (`// DIVERGES:` lines). Anything not listed there is a real defect: the
# gates compare a hash and are blind to exception text, `toString` formats and
# initialiser order, which is the whole reason these exist.
#
#   bench\sem_all.ps1                 # all of them
#   bench\sem_all.ps1 -Only CollSem   # one
param(
  [string]$Only = '',
  [string]$Exe = 'target\release\rava.exe',
  [int]$TimeoutS = 120
, [string]$Prefix = '')
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$work = Join-Path $env:TEMP 'rava_sem'
# MINECRAFT'S OWN LIBRARIES ON BOTH SIDES. A probe may exercise guava / fastutil / gson the way the
# server does (SortedSem: `ImmutableSortedMap.copyOf` order, which was wire-visibly wrong); javac
# and `java -Xint` see the same jars rava does, so a divergence is the engine's and never the
# classpath's. The JDK's own classes are not on this list, so the 21 probes that never touch a
# library resolve exactly as before.
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
if (-not $Prefix) { $Prefix = $mccp }
New-Item -ItemType Directory -Force -Path $work | Out-Null

function Invoke-Probe([string]$File, [string[]]$Arguments, [string]$Out, [string]$Err) {
  $quoted = @($Arguments | ForEach-Object { '"' + $_ + '"' })
  $process = Start-Process -FilePath $File -ArgumentList $quoted -WindowStyle Hidden `
    -PassThru -RedirectStandardOutput $Out -RedirectStandardError $Err
  if (-not $process.WaitForExit($TimeoutS * 1000)) {
    $process.Kill()
    $process.WaitForExit()
    return @{ ExitCode = 'TIMEOUT'; Text = (Get-Content $Out -Raw) }
  }
  return @{ ExitCode = $process.ExitCode; Text = (Get-Content $Out -Raw) }
}

$probes = Get-ChildItem 'bench\*Sem.java' | ForEach-Object { $_.BaseName }
if ($Only) { $probes = @($probes | Where-Object { $_ -eq $Only }) }
if (-not $probes.Count) { "no probe matched '$Only'"; exit 3 }

$fail = 0
foreach ($p in $probes) {
  $src = "bench\$p.java"
  # Compile once per probe. javac is the reference implementation's own front end, so a
  # probe that does not compile is a broken probe, not a finding about rava.
  $jc = & javac -cp $mccp -d $work $src 2>&1
  if ($LASTEXITCODE -ne 0) { "$p : JAVAC FAILED"; $jc | Select-Object -First 5; $fail++; continue }

  # STDOUT ONLY, on both sides. The first version piped stderr in too and every probe
  # came back "divergent" on rava's own `RAVA_PROC_CAP_MB` banner — plus four lines of
  # PowerShell's NativeCommandError wrapper around it, since 5.1 turns a native program's
  # stderr into ErrorRecords. The probes assert on what they PRINT; a banner is not a
  # semantic difference, and a harness that reports one is a harness that will be ignored.
  $rerr = Join-Path $work "$p.jvm.err"
  $gerr = Join-Path $work "$p.rava.err"
  $refRun = Invoke-Probe 'java' @('-Xint', '-cp', "$work;$mccp", $p) "$work\$p.jvm.out" $rerr
  $gotRun = Invoke-Probe $Exe @('-cp', "$(if ($Prefix) { "$Prefix;" })out;$work", $p) "$work\$p.rava.out" $gerr
  $ref = $refRun.Text
  $got = $gotRun.Text
  $refExit = $refRun.ExitCode
  $gotExit = $gotRun.ExitCode
  if ($refExit -ne 0 -or $gotExit -ne 0) {
    "$p : PROCESS FAILED (java=$refExit, rava=$gotExit)"
    $fail++
    continue
  }

  # Expected divergences are declared IN THE PROBE, next to the code that causes them,
  # so they cannot rot apart from it. A line is skipped on both sides if it starts with
  # any declared prefix.
  $skip = @(Get-Content $src | Where-Object { $_ -match '^//\s*DIVERGES:\s*(.+)$' } |
            ForEach-Object { $Matches[1].Trim() })
  $strip = {
    param($text)
    ($text -split "`r?`n") | Where-Object {
      $line = $_
      $line.Trim().Length -and -not ($skip | Where-Object { $line.StartsWith($_) })
    }
  }
  $a = & $strip $ref
  $b = & $strip $got
  # A TRUNCATED RUN IS A DIFFERENT FINDING FROM A DIVERGENT ONE, and it hides everything
  # behind it. bench/MapViewSem.java once reported "4 divergent lines" while rava's output
  # had simply ENDED at a missing native — twenty observables after that point had never
  # run, and the summary said nothing about it. Compare the COUNTS first and say so.
  if ($b.Count -lt $a.Count) {
    "$p : TRUNCATED — printed {0} of {1} lines; everything after line {0} was NOT TESTED" -f $b.Count, $a.Count
    $firstMissing = $a[$b.Count]
    "    stopped before:  $firstMissing"
    if (Test-Path $gerr) {
      Get-Content $gerr | Where-Object { $_ -match 'uncaught|Error|Exception' } |
        Select-Object -First 2 | ForEach-Object { "    stderr:  $_" }
    }
    $fail++
    continue
  }
  $d = Compare-Object $a $b
  if ($d) {
    "$p : {0} DIVERGENT LINE(S)" -f $d.Count
    $d | Select-Object -First 8 | ForEach-Object {
      $tag = if ($_.SideIndicator -eq '<=') { 'jvm ' } else { 'rava' }
      "    $tag  $($_.InputObject)"
    }
    $fail++
  } else {
    "$p : match ({0} lines, {1} declared divergence(s) skipped)" -f $a.Count, $skip.Count
  }
}
"---"
if ($fail) { "$fail of $($probes.Count) probe(s) DIVERGENT"; exit 1 }
"all $($probes.Count) probes match java -Xint"
exit 0

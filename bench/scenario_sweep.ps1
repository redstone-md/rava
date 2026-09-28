# WHERE DO WE STAND, ACROSS SCENARIOS, WITH BOTH COMPILERS OFF?
#
# ⭐**THE PROJECT HAS NEVER MEASURED THIS.** The standing goal is stated as "much faster in ALL
# scenarios, with no per-scenario tuning", and every number in the ledger comes from exactly one
# workload: the Minecraft `server.jar` boot. A single workload cannot distinguish "our interpreter
# is slow" from "this one boot does work the JVM skips" — and rows 366-376 showed those are very
# different problems with very different fixes.
#
# So this runs a spread of SELF-CONTAINED Java programs under `rava RAVA_JIT=0` and `java -Xint`,
# and reports the ratio per scenario.
#
# ⚠**OUTPUT IS COMPARED, NOT JUST TIME.** A speed ratio between two engines that computed different
# answers is not a measurement. A scenario whose stdout differs is reported as MISMATCH and its
# timing is discarded.
#
# ⚠**BUT THESE PROGRAMS PRINT THEIR OWN TIMINGS**, so a byte-exact comparison calls every scenario a
# mismatch — the first run of this script did exactly that, on all eight. Digits are therefore
# blanked before comparing: `ms=1234` and `ms=987` compare equal, while a difference in words,
# structure or line count still fails. That is weaker than the gates' hashes and is stated as such:
# this sweep answers "how fast, on comparable output", not "bit-exact".
#
# ⚠**Min-of-N, and N is small.** These are short programs on a shared box; the minimum is the least
# contaminated sample. Ratios near 1 should not be read to two decimals.
#
#   bench\scenario_sweep.ps1                 # the default spread
#   bench\scenario_sweep.ps1 -Runs 5
param(
  [string[]]$Names = @('ArithLoop', 'ArrayBench', 'CallBench', 'Allocs', 'B64', 'BigGraph', 'ArrReduce', 'BinarySearchLambda'),
  [int]$Runs = 3,
  [string]$Exe = 'target\release\rava.exe',
  [int]$TimeoutS = 300
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$outDir = 'out'
if (-not (Test-Path $outDir)) { New-Item -ItemType Directory $outDir | Out-Null }

function Run-One([string]$exe, [string[]]$argv, [string]$tag) {
  $log = "bench\sweep_$tag.log"
  Remove-Item $log, "$log.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $exe -ArgumentList $argv -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
  # Windows PowerShell 5.1 leaves ExitCode empty unless the handle is touched first.
  $null = $p.Handle
  if (-not $p.WaitForExit($TimeoutS * 1000)) {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    return @{ ms = $null; out = '<timeout>' }
  }
  $sw.Stop()
  $text = if (Test-Path $log) { (Get-Content $log -Raw) } else { '' }
  return @{ ms = $sw.Elapsed.TotalMilliseconds; out = $text; code = $p.ExitCode }
}

$rows = @()
foreach ($n in $Names) {
  if (-not (Test-Path "bench\$n.java")) { Write-Host "$n : no source"; continue }
  javac -nowarn -d $outDir "bench\$n.java" 2>&1 | Out-Null
  if (-not (Test-Path "$outDir\$n.class")) { Write-Host "$n : does not compile"; continue }

  $env:RAVA_JIT = '0'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  $rava = @(); $java = @(); $ro = ''; $jo = ''
  # `$ro`/`$jo` stay empty strings if a run produced nothing, so the compare below cannot null-deref.
  for ($i = 0; $i -lt $Runs; $i++) {
    # Alternate so a monotone drift of the box does not land on one engine.
    if ($i % 2 -eq 0) {
      $a = Run-One $Exe @('-cp', $outDir, $n) "rava_$n"; $b = Run-One 'java' @('-Xint', '-cp', $outDir, $n) "java_$n"
    } else {
      $b = Run-One 'java' @('-Xint', '-cp', $outDir, $n) "java_$n"; $a = Run-One $Exe @('-cp', $outDir, $n) "rava_$n"
    }
    if ($null -ne $a.ms) { $rava += $a.ms; if ($null -ne $a.out) { $ro = $a.out } }
    if ($null -ne $b.ms) { $java += $b.ms; if ($null -ne $b.out) { $jo = $b.out } }
  }
  if ($rava.Count -eq 0 -or $java.Count -eq 0) { Write-Host ("{0,-20} DID NOT RUN" -f $n); continue }
  # An engine that printed NOTHING did not run the scenario, it failed it. Reported as FAILED rather
  # than as a fast time: the first run of this sweep called an empty rava output "1.6x FASTER" on
  # B64, which was a `java.util.Base64` MIME decode throwing, not a win.
  if ($ro.Trim().Length -eq 0 -or $jo.Trim().Length -eq 0) {
    $who = if ($ro.Trim().Length -eq 0) { 'rava' } else { 'java' }
    Write-Host ("{0,-20} FAILED: {1} produced no output" -f $n, $who)
    continue
  }

  # Blank digits (these are benchmarks: they print elapsed times) and collapse whitespace, because
  # the two engines do not agree on line endings and a CRLF/LF difference is not a divergence. The
  # first two runs of this script reported MISMATCH on every scenario for that reason alone.
  $norm = { param($t) (($t -replace '[0-9]+', '#') -replace '\s+', ' ').Trim() }
  $same = ((& $norm $ro) -eq (& $norm $jo))
  $rm = ($rava | Measure-Object -Minimum).Minimum
  $jm = ($java | Measure-Object -Minimum).Minimum
  $ratio = if ($jm -gt 0) { $rm / $jm } else { 0 }
  $verdict = if (-not $same) { 'MISMATCH (timing discarded)' }
             elseif ($ratio -lt 1) { ('{0:N2}x FASTER' -f (1 / $ratio)) }
             else { ('{0:N2}x slower' -f $ratio) }
  Write-Host ("{0,-20} rava {1,8:N0} ms   java -Xint {2,8:N0} ms   {3}" -f $n, $rm, $jm, $verdict)
  $rows += [pscustomobject]@{ name = $n; rava = $rm; java = $jm; ratio = $ratio; same = $same }
}

$ok = @($rows | Where-Object { $_.same })
if ($ok.Count -eq 0) { Write-Host ''; Write-Host 'no comparable scenario'; exit 1 }
Write-Host ''
$faster = @($ok | Where-Object { $_.ratio -lt 1 }).Count
$geo = [Math]::Exp((($ok | ForEach-Object { [Math]::Log($_.ratio) }) | Measure-Object -Sum).Sum / $ok.Count)
Write-Host ("scenarios compared: {0}   rava faster in {1}   geometric mean ratio {2:N2}x" -f $ok.Count, $faster, $geo)
Write-Host ("worst: {0}" -f (($ok | Sort-Object ratio -Descending | Select-Object -First 1 | ForEach-Object { '{0} {1:N2}x' -f $_.name, $_.ratio })))
Write-Host ("best:  {0}" -f (($ok | Sort-Object ratio | Select-Object -First 1 | ForEach-Object { '{0} {1:N2}x' -f $_.name, $_.ratio })))

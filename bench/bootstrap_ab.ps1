# PAIRED SAME-BINARY FLAG A/B ON THE BOOT-CRITICAL POPULATION: Bootstrap.bootStrap() alone.
#
# WHY. The full-boot A/B has an ~8% noise floor on this box (a null test against a nonexistent flag
# once read 7.34% "disjoint"); every change this month prices at 1-4%. `cpu_ab.ps1` reads 1.2% arm
# spread, but only on synthetic programs that do not contain the boot population. `BootstrapBench`
# IS the boot population: registry <clinit> chain + block-state graph + initCache (41% of JIT_EXEC on
# a real boot lives inside this one call), timed inside the guest and content-hashed.
#
# WHAT IS MEASURED, per run: guest BOOTSTRAP_MS (fixed work, the primary signal), process CPU-ms,
# and QueryProcessCycleTime cycles (immune to frequency scaling / E-core migration). Wall is recorded
# only for the contention guard, never reported as a result.
#
# ⚠**VALIDATE AGAINST ITSELF FIRST**: run with a flag that does not exist (e.g. -Flag RAVA_NULL_TEST).
# An instrument that cannot read ~0 on two copies of one configuration cannot read anything else.
#
# ⚠**THE HASH IS REQUIRED.** A run whose line lacks HASH=<Want> is DISCARDED and the observed hash is
# printed, so a change that speeds the bootstrap up by breaking it cannot register as a win — and an
# engine that DISAGREES with java (which is a finding in itself) is named, not averaged in.
# The default -Want is java's own answer: `java -cp out;<mc-cp> BootstrapBench` printed
# STATES=29671 HASH=b9e7debe631b42b6 on 2026-09-02 (Temurin 25.0.3, five runs identical).
#
# ⚠**ONE bootstrap per process.** Bootstrap.bootStrap() is guarded by `isBootstrapped` (pc 0-6), so
# repetition happens by re-launching the process — that is what -Pairs does.
#
# Convention: A = flag UNSET (baseline), B = flag SET to '1'. Delta is reported as B vs A, so a
# negative number means the flag made it FASTER.
#
#   bench\bootstrap_ab.ps1 -Flag RAVA_NULL_TEST                 # null test first
#   bench\bootstrap_ab.ps1 -Flag RAVA_SOME_FLAG -Pairs 5
#   bench\bootstrap_ab.ps1 -Flag RAVA_SOME_FLAG -Jit 0          # interpreter configuration
param(
  [Parameter(Mandatory = $true)][string]$Flag,
  # Value the B arm sets the flag to (default '1'; e.g. -FlagValue 4 for a thread count).
  [string]$FlagValue = '1',
  [int]$Pairs = 5,
  [string]$Exe = 'target\release\rava.exe',
  [string]$Want = 'b9e7debe631b42b6',
  # Product configuration by default (the boot runs JIT ON). -Jit 0 measures the interpreter.
  [string]$Jit = '1',
  # Extra env "K=V;K=V" applied to BOTH arms.
  [string]$Extra = '',
  # Passed to BootstrapBench as --fold N (repeats the read-only hash fold; the bootstrap itself
  # still runs once).
  [int]$Fold = 1,
  # Skip the stale-binary guard (deliberate measurement of an older build).
  [switch]$AllowStale
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = 'out;' + $mccp
javac -nowarn -cp $mccp -d out bench\BootstrapBench.java 2>&1 | Out-Null
if (-not (Test-Path 'out\BootstrapBench.class')) { Write-Host 'javac failed: out\BootstrapBench.class missing'; exit 1 }

# Refuse to measure a stale binary (see assert_fresh_exe.ps1 for the three silent failures).
. "$PSScriptRoot\assert_fresh_exe.ps1"
if ($AllowStale) { Assert-FreshExe $Exe -WarnOnly } else { Assert-FreshExe $Exe }

$env:RAVA_JIT = $Jit; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
# Fresh-process evidence must never inherit a state-resume shortcut from the shell.
foreach ($k in 'RAVA_RESTORE', 'RAVA_CHECKPOINT_FILE', 'RAVA_CHECKPOINT_OPS', 'RAVA_STACKFUL', 'RAVA_FLAT_STAT') {
  Remove-Item "env:$k" -EA SilentlyContinue
}
if ($Extra) { foreach ($kv in $Extra.Split(';')) { if ($kv) { $p = $kv.Split('=', 2); Set-Item "env:$($p[0])" $p[1] } } }

# QueryProcessCycleTime: cycles actually executed by the process, across all its threads.
if (-not ('Win32Cycles' -as [type])) {
  Add-Type -Namespace '' -Name Win32Cycles -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError=true)]
public static extern bool QueryProcessCycleTime(IntPtr h, out ulong cycles);
'@
}

function One([string]$side) {
  if ($side -eq 'B') { Set-Item "env:$Flag" $FlagValue } else { Remove-Item "env:$Flag" -EA SilentlyContinue }
  $log = "bench\bootab_$side.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $argv = @('-cp', $cp, 'BootstrapBench')
  if ($Fold -gt 1) { $argv += @('--fold', "$Fold") }
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList $argv -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
  # PowerShell 5.1 leaves the process object half-initialised until the handle is touched.
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $cyc = [uint64]0
  # Sample the cycle counter just before exit: the handle dies with the process.
  while (-not $p.HasExited) {
    Start-Sleep -Milliseconds 100
    $c = [uint64]0
    try { if ([Win32Cycles]::QueryProcessCycleTime($p.Handle, [ref]$c)) { if ($c -gt $cyc) { $cyc = $c } } } catch {}
  }
  $wall = $sw.Elapsed.TotalMilliseconds
  $cpu = $p.TotalProcessorTime.TotalMilliseconds
  $text = if (Test-Path $log) { Get-Content $log -Raw } else { '' }
  if ($null -eq $text) { $text = '' }
  $m = [regex]::Match($text, 'BOOTSTRAP_MS=(\d+) STATES=(\d+) HASH=([0-9a-f]+)')
  # Name the reason for a discard: no line at all (crash / hang) is a different finding from a
  # wrong hash (disagreement with java), and both are findings.
  if (-not $m.Success) {
    $tail = if (Test-Path "$log.err") { (Get-Content "$log.err" -Tail 2) -join ' | ' } else { '' }
    Write-Host ("  {0}: DISCARDED (no BOOTSTRAP_MS line; exit={1}; stderr tail: {2})" -f $side, $p.ExitCode, $tail)
    return $null
  }
  if ($m.Groups[3].Value -ne $Want) {
    Write-Host ("  {0}: DISCARDED (HASH {1} != expected {2}; STATES={3}) -- engine DISAGREES with java" -f `
      $side, $m.Groups[3].Value, $Want, $m.Groups[2].Value)
    return $null
  }
  $v = [pscustomobject]@{ ms = [double]$m.Groups[1].Value; cpu = $cpu; cyc = $cyc; wall = $wall }
  Write-Host ("  {0}: BOOTSTRAP_MS={1:N0}  cpu_ms={2:N0}  cycles={3:N0}" -f $side, $v.ms, $v.cpu, $v.cyc)
  return $v
}

$ra = @(); $rb = @()
for ($i = 0; $i -lt $Pairs; $i++) {
  # ABBA, so a monotone drift of the box cannot land on one arm.
  $order = if ($i % 2 -eq 0) { @('A', 'B') } else { @('B', 'A') }
  foreach ($s in $order) {
    $v = One $s
    if ($null -ne $v) { if ($s -eq 'A') { $ra += $v } else { $rb += $v } }
  }
}
Remove-Item "env:$Flag" -EA SilentlyContinue
if ($ra.Count -eq 0 -or $rb.Count -eq 0) { Write-Host 'no valid pairs'; exit 1 }

function Median($v) {
  $s = @($v | Sort-Object); $n = $s.Count
  if ($n % 2 -eq 1) { return $s[[math]::Floor($n / 2)] } else { return ($s[$n / 2 - 1] + $s[$n / 2]) / 2 }
}
function Report($name, $av, $bv) {
  $am = ($av | Measure-Object -Minimum).Minimum; $bm = ($bv | Measure-Object -Minimum).Minimum
  $ax = ($av | Measure-Object -Maximum).Maximum; $bx = ($bv | Measure-Object -Maximum).Maximum
  $amed = Median $av; $bmed = Median $bv
  $verdict = if ($bx -lt $am) { 'every B below every A (DISJOINT, flag faster)' }
             elseif ($bm -gt $ax) { 'every B above every A (DISJOINT, flag slower)' }
             else { 'arms overlap' }
  $spreadA = if ($am -gt 0) { ($ax - $am) * 100 / $am } else { 0 }
  Write-Host ("  {0,-12} A(unset) {1}" -f $name, (($av | ForEach-Object { '{0:N0}' -f $_ }) -join ' / '))
  Write-Host ("  {0,-12} B(set)   {1}" -f '', (($bv | ForEach-Object { '{0:N0}' -f $_ }) -join ' / '))
  Write-Host ("  {0,-12} min B vs A {1:N2}%   median B vs A {2:N2}%   A-arm spread {3:N1}%   {4}" -f '', `
    (($bm - $am) * 100 / $am), (($bmed - $amed) * 100 / $amed), $spreadA, $verdict)
}
Write-Host ("BootstrapBench / {0}  (RAVA_JIT={1}; guest ms, CPU-ms and cycles; wall not reported)" -f $Flag, $Jit)
Report 'BOOTSTRAP_MS' ($ra.ms)  ($rb.ms)
Report 'cpu_ms'       ($ra.cpu) ($rb.cpu)
Report 'cycles'       ($ra.cyc) ($rb.cyc)

# PAIRED VIEW on the guest metric: consecutive A/B runs see almost the same machine, so their
# difference cancels drift that per-arm min/median cannot.
$n = [math]::Min($ra.Count, $rb.Count)
if ($n -ge 2) {
  # Scalar casts on purpose: PowerShell 5.1 hands `[math]::Round` an Object[] here when a run
  # object arrives wrapped, and the whole summary then dies on the conversion.
  # Member enumeration (`$ra.ms`) is what `Report` already trusts; indexing `$ra[$i]` handed back a
  # wrapped run object and the paired list collapsed to one number.
  $ams = @($ra.ms); $bms = @($rb.ms); $n = [math]::Min($ams.Count, $bms.Count)
  $pairs = @(); for ($i = 0; $i -lt $n; $i++) { $pairs += [math]::Round([double]$bms[$i] - [double]$ams[$i]) }
  $neg = @($pairs | Where-Object { $_ -lt 0 }).Count
  Write-Host ("  PAIRED BOOTSTRAP_MS (B minus A, by position; negative = flag faster): {0}   median {1}   {2}/{3} favour flag" -f `
    ($pairs -join '  '), (Median $pairs), $neg, $n)
}

# CONTENTION GUARD, against the batch: any run >25% above the fastest wall saw a different machine.
$walls = @($ra.wall) + @($rb.wall)
$wmin = ($walls | Measure-Object -Minimum).Minimum
$slow = @($walls | Where-Object { $_ -gt $wmin * 1.25 })
if ($slow.Count) {
  Write-Host ("*** BATCH NOT LEVEL: {0} of {1} runs are >25% above the fastest wall ({2:N0} ms). Whatever the arms differ by, it is not only the flag." -f `
    $slow.Count, $walls.Count, $wmin) -ForegroundColor Red
}

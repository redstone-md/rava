# PAIRED SAME-BINARY FLAG A/B ON THE DATAPACK-RELOAD POPULATION: WorldLoader.load's reload alone.
#
# WHY. bootstrap_ab.ps1 prices a flag on Bootstrap.bootStrap() (start -> `Environment:`). The NEXT
# window of a boot, `Environment:` -> `Loaded 1470 recipes`, is 13-17 s on rava vs 2 s on java and
# had no instrument. `ReloadBench` IS that population: the vanilla datapack reload (tags, worldgen +
# dimension registries through RegistryDataLoader, loot tables, recipes, advancements, functions,
# commands) on Util.backgroundExecutor() plus the main-thread queue drain, exactly as
# net.minecraft.server.Main runs it, headless, no world, no network -- timed inside the guest and
# content-hashed.
#
# WHAT IS MEASURED, per run: guest RELOAD_MS (loadResources + drain; fixed work, the primary signal),
# guest WINDOW_MS (all of WorldLoader.load: registries + reload), process CPU-ms, and
# QueryProcessCycleTime cycles (immune to frequency scaling / E-core migration). Wall is recorded
# only for the contention guard, never reported as a result.
#
# ⚠**VALIDATE AGAINST ITSELF FIRST**: run with a flag that does not exist (e.g. -Flag RAVA_NULL_TEST).
# An instrument that cannot read ~0 on two copies of one configuration cannot read anything else.
#
# ⚠**THE HASH IS REQUIRED.** A run whose line lacks HASH=<Want> is DISCARDED and the observed hash is
# printed, so a change that speeds the reload up by breaking it cannot register as a win — and an
# engine that DISAGREES with java (which is a finding in itself) is named, not averaged in.
# The default -Want is java's own answer: `java -cp out;<mc-cp> ReloadBench` printed
# RECIPES=1470 ADVANCEMENTS=1584 TAGS=625 HASH=c956377e19071d7a on 2026-09-02 (Temurin 25.0.3,
# three runs identical; RELOAD_MS 1319-1678, WINDOW_MS 2773-3163).
#
# Each process measures one cold reload and WarmReloads warm reloads (default one).
# Every warm resource set must match the cold digest and counts, outside its timed interval.
# With multiple warm reloads, validation between iterations can affect later cache/JIT state.
# -Pairs repeats fresh processes; RELOAD2_MS reports only the first warm interval.
#
# Convention: A = flag UNSET (baseline), B = flag SET to '1'. Delta is reported as B vs A, so a
# negative number means the flag made it FASTER.
#
#   bench\reload_ab.ps1 -Flag RAVA_NULL_TEST                 # null test first
#   bench\reload_ab.ps1 -Flag RAVA_SOME_FLAG -Pairs 5
#   bench\reload_ab.ps1 -Flag RAVA_SOME_FLAG -Jit 0          # interpreter configuration
param(
  [Parameter(Mandatory = $true)][string]$Flag,
  # Value the B arm sets the flag to (default '1'; e.g. -FlagValue 4 for a thread count).
  [string]$FlagValue = '1',
  [int]$Pairs = 5,
  [ValidateRange(1, 100)][int]$WarmReloads = 1,
  [string]$Exe = 'target\release\rava.exe',
  [string]$Want = 'c956377e19071d7a',
  # Product configuration by default (the boot runs JIT ON). -Jit 0 measures the interpreter.
  [string]$Jit = '1',
  # Extra env "K=V;K=V" applied to BOTH arms.
  [string]$Extra = '',
  [int]$TimeoutS = 600,
  [string]$EvidenceDir = '',
  # Skip the stale-binary guard (deliberate measurement of an older build).
  [switch]$AllowStale
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
if (-not $EvidenceDir) {
  $EvidenceDir = Join-Path $env:TEMP ('rava-reload-ab-' + [guid]::NewGuid().ToString('N'))
}
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$EvidenceDir = (Resolve-Path -LiteralPath $EvidenceDir).Path
Get-FileHash -LiteralPath $Exe | Format-List | Out-File "$EvidenceDir\binary.txt"
if (Test-Path -LiteralPath 'rava.toml') {
  Copy-Item -LiteralPath 'rava.toml' -Destination "$EvidenceDir\rava.toml"
}
Write-Host "Raw evidence: $EvidenceDir"
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = 'out;' + $mccp
javac -nowarn -cp $mccp -d out bench\ReloadBench.java 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0 -or -not (Test-Path 'out\ReloadBench.class')) { Write-Host 'javac failed'; exit 1 }

# Refuse to measure a stale binary (see assert_fresh_exe.ps1 for the three silent failures).
. "$PSScriptRoot\assert_fresh_exe.ps1"
if ($AllowStale) { Assert-FreshExe $Exe -WarnOnly } else { Assert-FreshExe $Exe }

$env:RAVA_JIT = $Jit; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
# Fresh-process evidence must never inherit a state-resume shortcut from the shell.
if ($Extra) { foreach ($kv in $Extra.Split(';')) { if ($kv) { $p = $kv.Split('=', 2); Set-Item "env:$($p[0])" $p[1] } } }
Get-ChildItem Env:RAVA* | Where-Object {
  $_.Name -eq 'RAVA_RESTORE' -or $_.Name -like 'RAVA_CHECKPOINT*' -or
  $_.Name -in @('RAVA_STACKFUL', 'RAVA_FLAT_STAT')
} | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }

# QueryProcessCycleTime: cycles actually executed by the process, across all its threads.
if (-not ('Win32Cycles' -as [type])) {
  Add-Type -Namespace '' -Name Win32Cycles -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError=true)]
public static extern bool QueryProcessCycleTime(IntPtr h, out ulong cycles);
'@
}

$script:RunSeq = 0
Remove-Item 'bench\reloadab_discard_*' -ErrorAction SilentlyContinue
function One([string]$side) {
  $script:RunSeq++
  if ($side -eq 'B') { Set-Item "env:$Flag" $FlagValue } else { Remove-Item "env:$Flag" -EA SilentlyContinue }
  $log = Join-Path $EvidenceDir ("run_{0}_{1}.log" -f $script:RunSeq, $side)
  Get-ChildItem Env:RAVA* | Sort-Object Name | Format-Table -AutoSize | Out-File "$log.env"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $argv = @('-cp', $cp, 'ReloadBench', '--reloads', "$WarmReloads")
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList $argv -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
  # PowerShell 5.1 leaves the process object half-initialised until the handle is touched.
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $cyc = [uint64]0
  $timedOut = $false
  # Sample the cycle counter just before exit: the handle dies with the process.
  while (-not $p.HasExited) {
    if ($sw.Elapsed.TotalSeconds -gt $TimeoutS) {
      $timedOut = $true
      $p.Kill()
      $p.WaitForExit()
      break
    }
    Start-Sleep -Milliseconds 100
    $c = [uint64]0
    try { if ([Win32Cycles]::QueryProcessCycleTime($p.Handle, [ref]$c)) { if ($c -gt $cyc) { $cyc = $c } } } catch {}
  }
  $wall = $sw.Elapsed.TotalMilliseconds
  $cpu = $p.TotalProcessorTime.TotalMilliseconds
  [pscustomobject]@{ sequence = $script:RunSeq; side = $side; exit = $p.ExitCode;
    timeout = $timedOut; wall_ms = $wall; cpu_ms = $cpu; cycles = $cyc } |
    ConvertTo-Json -Compress | Add-Content "$EvidenceDir\runs.jsonl"
  $text = if (Test-Path $log) { Get-Content $log -Raw } else { '' }
  if ($null -eq $text) { $text = '' }
  # Fields between RELOAD_MS and RECIPES are optional (RELOAD2_MS, RELOADS=[..]) so the contract
  # survives the bench gaining metrics. RELOAD2_MS -- a SECOND, warm reload over the same
  # ResourceManager -- is the metric to price on: RELOAD_MS carries the whole first-pass JIT
  # warm-up (java 594 ms then 192/148/148/144; rava 9,162 then ~7,200, flat).
  $m = [regex]::Match($text, 'RELOAD_MS=(\d+).*? RECIPES=(\d+) ADVANCEMENTS=(\d+) TAGS=(\d+) HASH=([0-9a-f]+)')
  # Name the reason for a discard: no line at all (crash / hang) is a different finding from a
  # wrong hash (disagreement with java), and both are findings.
  if ($timedOut -or $p.ExitCode -ne 0 -or -not $m.Success) {
    $tail = if (Test-Path "$log.err") { (Get-Content "$log.err" -Tail 2) -join ' | ' } else { '' }
    $discard = "bench\reloadab_discard_$($script:RunSeq)_$side"
    if (Test-Path $log) { Copy-Item $log "$discard.log" }
    if (Test-Path "$log.err") { Copy-Item "$log.err" "$discard.log.err" }
    Write-Host ("  {0}: DISCARDED (failed process or missing result; exit={1}; timeout={4}; stderr tail: {2}; saved={3})" -f $side, $p.ExitCode, $tail, $discard, $timedOut)
    return $null
  }
  if ($m.Groups[5].Value -ne $Want) {
    Write-Host ("  {0}: DISCARDED (HASH {1} != expected {2}; RECIPES={3} ADVANCEMENTS={4} TAGS={5}) -- engine DISAGREES with java" -f `
      $side, $m.Groups[5].Value, $Want, $m.Groups[2].Value, $m.Groups[3].Value, $m.Groups[4].Value)
    return $null
  }
  $warmResults = [regex]::Matches($text, '(?m)^WARM_RESULT index=(\d+) RECIPES=(\d+) ADVANCEMENTS=(\d+) TAGS=(\d+) HASH=([0-9a-f]+)\r?$')
  if ($warmResults.Count -ne $WarmReloads) {
    Write-Host "  ${side}: DISCARDED (expected $WarmReloads certified warm results, got $($warmResults.Count))"
    return $null
  }
  for ($r = 0; $r -lt $WarmReloads; $r++) {
    $result = $warmResults[$r]
    if ([int]$result.Groups[1].Value -ne $r -or $result.Groups[5].Value -ne $Want -or
        $result.Groups[2].Value -ne $m.Groups[2].Value -or
        $result.Groups[3].Value -ne $m.Groups[3].Value -or
        $result.Groups[4].Value -ne $m.Groups[4].Value) {
      Write-Host "  ${side}: DISCARDED (warm result $r differs from required content)"
      return $null
    }
  }
  # WINDOW_MS (all of WorldLoader.load) follows HASH on the same line; absent only on an older bench.
  $w = [regex]::Match($text, 'WINDOW_MS=(\d+)')
  $w2 = [regex]::Match($text, 'RELOAD2_MS=(\d+)')
  $win = if ($w.Success) { [double]$w.Groups[1].Value } else { [double]$m.Groups[1].Value }
  $warm = if ($w2.Success) { [double]$w2.Groups[1].Value } else { 0 }
  $v = [pscustomobject]@{ ms = [double]$m.Groups[1].Value; win = $win; cpu = $cpu; cyc = $cyc; wall = $wall; warm = $warm }
  Write-Host ("  {0}: RELOAD_MS={1:N0}  WINDOW_MS={2:N0}  cpu_ms={3:N0}  cycles={4:N0}" -f $side, $v.ms, $v.win, $v.cpu, $v.cyc)
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
if ($ra.Count -ne $Pairs -or $rb.Count -ne $Pairs) {
  Write-Host 'INVALID BATCH: a run failed; do not re-pair surviving runs or infer a speedup.'
  exit 1
}

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
Write-Host ("ReloadBench / {0}  (RAVA_JIT={1}; guest ms, CPU-ms and cycles; wall not reported)" -f $Flag, $Jit)
Report 'RELOAD_MS'    ($ra.ms)  ($rb.ms)
Report 'WINDOW_MS'    ($ra.win) ($rb.win)
if (@($ra.warm | Where-Object { $_ -gt 0 }).Count) { Report 'RELOAD2_MS' ($ra.warm) ($rb.warm) }  # the warm reload: the tightest metric this bench has
Report 'cpu_ms'       ($ra.cpu) ($rb.cpu)
Report 'cycles'       ($ra.cyc) ($rb.cyc)

# PAIRED VIEW on the guest metric: consecutive A/B runs see almost the same machine, so their
# difference cancels drift that per-arm min/median cannot.
$n = [math]::Min($ra.Count, $rb.Count)
if ($n -ge 2) {
  # PowerShell variable names are case-insensitive: `$pairs` aliases the typed `[int]$Pairs`
  # parameter and rejects an Object[] assignment. Use a distinct name for the delta vector.
  # Member enumeration (`$ra.ms`) is what `Report` already trusts; indexing `$ra[$i]` hands back a
  # wrapped run object.
  $ams = @($ra.ms); $bms = @($rb.ms); $n = [math]::Min($ams.Count, $bms.Count)
  $pairedDeltas = @()
  for ($i = 0; $i -lt $n; $i++) {
    $pairedDeltas += [math]::Round([Convert]::ToDouble($bms[$i]) - [Convert]::ToDouble($ams[$i]))
  }
  $neg = @($pairedDeltas | Where-Object { $_ -lt 0 }).Count
  Write-Host ("  PAIRED RELOAD_MS (B minus A, by position; negative = flag faster): {0}   median {1}   {2}/{3} favour flag" -f `
    ($pairedDeltas -join '  '), (Median $pairedDeltas), $neg, $n)
}

# CONTENTION GUARD, against the batch: any run >25% above the fastest wall saw a different machine.
$walls = @($ra.wall) + @($rb.wall)
$wmin = ($walls | Measure-Object -Minimum).Minimum
$slow = @($walls | Where-Object { $_ -gt $wmin * 1.25 })
if ($slow.Count) {
  Write-Host ("*** BATCH NOT LEVEL: {0} of {1} runs are >25% above the fastest wall ({2:N0} ms). Whatever the arms differ by, it is not only the flag." -f `
    $slow.Count, $walls.Count, $wmin) -ForegroundColor Red
}

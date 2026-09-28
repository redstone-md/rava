# Interleaved ABBA A/B on the FULL server boot, primary metric = WALL (sub-second).
#
# Wall here = stopwatch from process start until the "Done (" line appears in the
# server log, polled at 250 ms. The log's own timestamps are 1 s granular, which is
# too coarse for a wall verdict, so the stopwatch is authoritative and the parsed
# per-phase splits are secondary.
#
#   .\bench\wall_th_ab.ps1 -Pairs 3 -AEnv "" -BEnv "RAVA_JIT_THRESHOLD=16" -Tag th16
#
# A run that fails to reach Done is INVALID and reported as such, never averaged in.
param(
  [int]$Pairs = 3,
  [string]$AEnv = "",
  [string]$BEnv = "RAVA_JIT_THRESHOLD=16",
  [string]$AExe = "target\release\rava.exe",
  [string]$BExe = "target\release\rava.exe",
  [string]$Tag = "ab",
  [int]$TimeoutSec = 200
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..

$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

function Invoke-Boot {
  param([string]$Arm, [string]$EnvSpec, [int]$Idx, [string]$Exe)

  # Fixed env for every arm; only $EnvSpec differs. EVERY name either arm mentions is
  # cleared first — a hardcoded RAVA_* list silently leaked MIMALLOC_ALLOW_LARGE_OS_PAGES
  # from the B arm into all seven later runs of a batch, which then failed identically
  # and read like a product bug in BOTH arms.
  Remove-Item Env:RAVA_JIT_THRESHOLD,Env:RAVA_NATIVE_LOADS,Env:RAVA_VERIFY_LOADS,`
              Env:RAVA_PROFILE_WALL,Env:RAVA_PROFILE_PC_STACKS,Env:RAVA_JIT_STATS,`
              Env:RAVA_COMPILE_TIME -ErrorAction SilentlyContinue
  foreach ($spec in @($AEnv, $BEnv)) {
    if (-not $spec) { continue }
    foreach ($pair in $spec.Split(',')) {
      if ($pair -match '^([^=]+)=') { Remove-Item "Env:$($Matches[1])" -ErrorAction SilentlyContinue }
    }
  }
  $env:RAVA_JIT = '1'
  if ($EnvSpec) {
    foreach ($pair in $EnvSpec.Split(',')) {
      if ($pair -match '^([^=]+)=(.*)$') { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
    }
  }

  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null

  $out = "bench\wall_${Tag}_${Arm}$Idx.log"
  $err = "$out.err"
  Remove-Item $out,$err -ErrorAction SilentlyContinue

  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError $err -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  # HIGH priority (base 13) so an unrelated Normal-priority process — a game, a
  # browser, an indexer — cannot deschedule the run. Affinity and the power plan do
  # NOT cover this: on 2026-08-04 a Normal-priority game holding ~2.9 cores moved the
  # SAME binary from 71.2 s to 88.3 s and produced two fake "ring collapses"
  # (cpu ~= wall). External CPU-seconds are recorded per run so a polluted sample is
  # visible in the data instead of being averaged in silently.
  $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
  $cpuAll0 = ((Get-Process).CPU | Measure-Object -Sum).Sum

  $wall = $null; $peak = 0
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    Start-Sleep -Milliseconds 250
    if (-not $p.HasExited) { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } }
    if (Test-Path $out) {
      $tail = Get-Content $out -Tail 3 -ErrorAction SilentlyContinue
      if ($tail -match 'Done \(') { $wall = $sw.Elapsed.TotalSeconds; break }
    }
    if ($p.HasExited) { break }
  }
  $cpu = $null
  if (-not $p.HasExited) { try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}; $p.Kill(); $p.WaitForExit() }
  $cpuAll1 = ((Get-Process).CPU | Measure-Object -Sum).Sum
  $ext = [math]::Round(($cpuAll1 - $cpuAll0) - $(if ($cpu) { $cpu } else { 0 }), 1)

  $log = Get-Content $out -ErrorAction SilentlyContinue
  $done = $null
  $m = $log | Select-String 'Done \(([\d.]+)s\)' | Select-Object -First 1
  if ($m) { $done = [double]$m.Matches[0].Groups[1].Value }

  # Secondary 1 s-granular phase splits from the log's own timestamps.
  function Stamp([string]$pat) {
    $h = $log | Select-String $pat | Select-Object -First 1
    if ($h) { [datetime]::ParseExact($h.Matches[0].Groups[1].Value, 'HH:mm:ss', $null) } else { $null }
  }
  $t0   = Stamp '\[(\d{2}:\d{2}:\d{2})\]'
  $tenv = Stamp '\[(\d{2}:\d{2}:\d{2})\] \[ServerMain/INFO\]: Environment'
  $trec = Stamp '\[(\d{2}:\d{2}:\d{2})\].*Loaded 1470 recipes'
  # The server log stamps `HH:mm:ss` with no date, so a boot that crosses midnight
  # subtracts to a large NEGATIVE span (one run printed `ab=-86381`). A phase is never
  # negative and never longer than the run, so wrap it forward by a day.
  # Params are [object], NOT [datetime]: the null guard below is unreachable with a typed
  # parameter, because PowerShell rejects the null during argument transformation and throws
  # before the body runs. A run that never printed `Environment:` (a boot that died early —
  # exactly the case an A/B must survive to report) then killed the whole batch instead of
  # being recorded as INVALID.
  function Span([object]$from, [object]$to) {
    if ($null -eq $from -or $null -eq $to) { return $null }
    $s = ($to - $from).TotalSeconds
    if ($s -lt 0) { $s += 86400 }
    $s
  }
  $ab = Span $t0 $tenv
  $c  = Span $tenv $trec

  [pscustomobject]@{
    Arm = $Arm; Idx = $Idx; Valid = ($null -ne $wall)
    Wall = $wall; Done = $done; Cpu = $cpu; PeakMB = [math]::Round($peak/1MB, 0)
    ExtCpu = $ext; AB = $ab; C = $c; Log = $out
  }
}

$rows = @()
for ($i = 1; $i -le $Pairs; $i++) {
  # ABBA: pair 1 runs A then B, pair 2 runs B then A, ... cancels monotone drift.
  $order = if ($i % 2 -eq 1) { @(@('A',$AEnv,$AExe), @('B',$BEnv,$BExe)) } else { @(@('B',$BEnv,$BExe), @('A',$AEnv,$AExe)) }
  foreach ($o in $order) {
    $r = Invoke-Boot -Arm $o[0] -EnvSpec $o[1] -Idx $i -Exe $o[2]
    $rows += $r
    if ($r.Valid) {
      Write-Host ("{0}{1}  wall={2,6:N2}s  done={3,7}  cpu={4,7:N1}  extcpu={5,6}  ab={6}  c={7}  rss={8}MB" -f `
        $r.Arm, $r.Idx, $r.Wall, $r.Done, $r.Cpu, $r.ExtCpu, $r.AB, $r.C, $r.PeakMB)
    } else {
      Write-Host ("{0}{1}  INVALID (no Done) -> {2}" -f $r.Arm, $r.Idx, $r.Log)
    }
  }
}

$rows | Export-Csv "bench\wall_${Tag}_ab.csv" -NoTypeInformation
foreach ($arm in 'A','B') {
  $v = $rows | Where-Object { $_.Arm -eq $arm -and $_.Valid }
  if ($v) {
    $w = ($v.Wall | Measure-Object -Average -Minimum -Maximum)
    Write-Host ("{0}: n={1}/{2}  wall avg={3:N2}  min={4:N2}  max={5:N2}" -f `
      $arm, $v.Count, ($rows | Where-Object Arm -eq $arm).Count, $w.Average, $w.Minimum, $w.Maximum)
  } else {
    Write-Host ("{0}: NO VALID RUNS" -f $arm)
  }
}

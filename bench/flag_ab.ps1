# PAIRED A/B of an ENV FLAG on one binary.
#
# `wt_boot_ab.ps1` alternates two BINARIES; this alternates two ENVIRONMENTS on
# the same one, which is what a default-off feature flag needs and what makes the
# experiment cost a boot instead of a build. Same alternation, same world
# restore, same quiet check, same self-judging batch — the only difference is
# what varies between the arms.
#
#   bench\flag_ab.ps1 -Flag RAVA_DISPATCH_TABLE=1 -N 3
param(
  [string]$Flag = '',
  [int]$N = 3,
  [string]$Exe = 'target\pgo-fat\rava.exe',
  [int]$TimeoutS = 300,
  # How many times to wait out a busy box before refusing. A refusal is not a result,
  # and this was the THIRD harness to learn it: `gate_ab` grew a private wait loop,
  # `wt_boot_ab` lost an unattended batch without one, and this one threw away a
  # 35-minute run to `queue 3` raised by the desktop's own applications.
  [int]$Tries = 10
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
. "$PSScriptRoot\require_quiet.ps1"
Wait-QuietBox -Context 'the flag A/B' -Tries $Tries
if (-not $Flag) { 'need -Flag NAME=VALUE'; exit 3 }
. "$PSScriptRoot\assert_flag.ps1"
Assert-FlagKnown -Exe $Exe -Flag $Flag
$kv = $Flag.Split('=', 2)
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'; $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$script:walls = @()
$res = @{ off = @(); on = @() }
"flag = $Flag   exe = $Exe"
foreach ($i in 1..$N) {
  # ABBA, for the reason `wt_boot_ab.ps1` runs it: a self-A/B of that harness against a
  # byte-identical copy of the same binary read the SECOND arm as +3.56% slower on `Done`
  # with 1 of 6 pairs going the other way. That is a positional penalty, and pairing does
  # not touch it — only alternating the order does. A flag A/B is the tree's most trusted
  # design (ledger row 269); it has no business carrying a bias the boot harness fixed.
  $order = if ($i % 2 -eq 1) { @('off', 'on') } else { @('on', 'off') }
  foreach ($side in $order) {
    if ($side -eq 'on') { Set-Item "env:$($kv[0])" $kv[1] } else { Remove-Item "env:$($kv[0])" -ErrorAction SilentlyContinue }
    robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
    $out = "bench\flag_$side.log"
    Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
          -WorkingDirectory (Get-Location) -RedirectStandardOutput $out -RedirectStandardError "$out.err" `
          -PassThru -WindowStyle Hidden
    $p.ProcessorAffinity = [IntPtr]0x0FFF
    $dl = [DateTime]::Now.AddSeconds($TimeoutS)
    $peak = 0
    while ([DateTime]::Now -lt $dl) {
      Start-Sleep -Milliseconds 200
      if ($p.HasExited) { break }
      try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
      if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
    }
    $sw.Stop()
    # CPU time — see the note in wt_boot_ab.ps1: it measures the work THIS process
    # did, so it is not lengthened by whatever else the box is running, and it is
    # the metric a "does this do less work?" question actually asks.
    $cpu = 0.0
    try { $p.Refresh(); $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
    if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
    $done = (Get-Content $out | Select-String 'Done \(([0-9.]+)s\)')
    $d = if ($done) { [double]$done.Matches[0].Groups[1].Value } else { -1 }
    # PHASE MAP, for the reason `wt_boot_ab.ps1` prints one: `Done` is the guest's own
    # clock and covers the LAST ~15 s of a ~46 s run, so a flag whose effect lands before
    # Minecraft's first line was invisible in both numbers this printed. Same marker set,
    # same whole-second resolution — coarse per run, meaningful over a batch.
    $phase = [ordered]@{ }
    $t0 = $null
    $seen = @{ }
    foreach ($line in (Get-Content $out)) {
      if ($line -notmatch '^\[(\d\d):(\d\d):(\d\d)\]') { continue }
      $sec = [int]$Matches[1] * 3600 + [int]$Matches[2] * 60 + [int]$Matches[3]
      if ($null -eq $t0) { $t0 = $sec }
      foreach ($mark in @(
          @{ k = 'env';      pat = 'Environment:' },
          @{ k = 'starting'; pat = 'Starting minecraft server' },
          @{ k = 'done';     pat = 'Done (' })) {
        if (-not $seen[$mark.k] -and $line.Contains($mark.pat)) {
          $seen[$mark.k] = $true
          $phase[$mark.k] = $sec - $t0
        }
      }
    }
    $script:walls += $sw.Elapsed.TotalSeconds
    $rss = [math]::Round($peak / 1MB)
    $res[$side] += [PSCustomObject]@{ wall = $sw.Elapsed.TotalSeconds; done = $d; rss = $rss; cpu = $cpu; phase = $phase }
    $blocks = if ($phase.Count -ge 3) {
      "  [boot {0}s | reg {1}s | prep {2}s]" -f $phase['env'],
        ($phase['starting'] - $phase['env']), ($phase['done'] - $phase['starting'])
    } else { '' }
    "{0,-4} run {1}: wall={2,6:N2}s cpu={3,6:N2}s done={4,6:N2}s rss={5,5}MB{6}" -f $side, $i, $sw.Elapsed.TotalSeconds, $cpu, $d, $rss, $blocks
  }
}
Remove-Item "env:$($kv[0])" -ErrorAction SilentlyContinue
$notLevel = $false
if ($script:walls.Count -ge 3) {
  $wmin = ($script:walls | Measure-Object -Minimum).Minimum
  $slow = @($script:walls | Where-Object { $_ -gt $wmin * 1.25 })
  if ($slow.Count) {
    "*** BATCH NOT LEVEL: {0} of {1} runs are >25% above the fastest ({2}s)." -f $slow.Count, $script:walls.Count, [math]::Round($wmin, 1)
    $notLevel = $true
  }
}
foreach ($side in 'off', 'on') {
  $w = ($res[$side] | Measure-Object wall -Average).Average
  $mn = ($res[$side] | Measure-Object wall -Minimum).Minimum
  $dn = ($res[$side] | Measure-Object done -Average).Average
  $rs = ($res[$side] | Measure-Object rss -Maximum).Maximum
  $cw = ($res[$side] | Measure-Object cpu -Average).Average
  $cmn = ($res[$side] | Measure-Object cpu -Minimum).Minimum
  "{0,-4} mean wall={1,6:N2}s min={2,6:N2}s | mean cpu={3,7:N2}s min={4,7:N2}s | mean done={5,6:N2}s peak rss={6,5}MB n={7}" -f $side, $w, $mn, $cw, $cmn, $dn, $rs, $res[$side].Count
}
if ($res['off'] | Where-Object { $_.done -lt 0 }) { exit 2 }
if ($res['on'] | Where-Object { $_.done -lt 0 }) { exit 2 }
if ($notLevel) { exit 1 }
exit 0

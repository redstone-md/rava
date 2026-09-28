# TWO-BINARY boot A/B: build any git ref in a worktree and boot both engines alternately on the
# same restored world. Wall, the guest's own Done, and PEAK WORKING SET, because a change can buy
# time with memory and this tree has a hard RSS budget (RAVA_HARD_MB=3584).
#
# Born as refactor_ab.ps1 — "did the module split cost anything?" — which is one instance of the
# general question. Same build profile both sides; the gates prove the OUTPUT is bit-exact and say
# nothing about what it costs to produce.
#
#   bench\wt_boot_ab.ps1 -Base HEAD -N 2
#   bench\wt_boot_ab.ps1 -BeforeExe bench\bin\a.exe -AfterExe bench\bin\b.exe -N 3
#
# `-BeforeExe`/`-AfterExe` skip the worktree build entirely, which is what makes
# BISECTION cheap: build each commit once into `bench/bin/`, then A/B any pair of
# them without rebuilding either. The alternation, the world restore and the quiet
# check are the point of this harness and apply the same way whichever binaries it
# is handed.
param([string]$Base = 'HEAD', [int]$N = 2, [string]$BeforeExe = '', [string]$AfterExe = '',
       # How many times to wait out a busy box before refusing. A refusal is not a result.
       [int]$Tries = 10)
Set-Location $PSScriptRoot\..
. "$PSScriptRoot\require_quiet.ps1"
Wait-QuietBox -Context 'the two-binary boot A/B' -Tries $Tries
$script:walls = @()
$wt = Join-Path $env:TEMP 'rava-refactor-ab'
if (-not $BeforeExe) {
  if (-not (Test-Path $wt)) { git worktree add $wt $Base | Out-Null }
  Push-Location $wt
  cargo build -p rava-cli --features next-gen-gc --release 2>&1 | Select-Object -Last 1
  Pop-Location
}
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$exes = @{
  before = if ($BeforeExe) { $BeforeExe } else { Join-Path $wt 'target\release\rava.exe' }
  after  = if ($AfterExe)  { $AfterExe }  else { 'target\release\rava.exe' }
}
foreach ($side in 'before', 'after') {
  if (-not (Test-Path $exes[$side])) { "missing $side binary: {0}" -f $exes[$side]; exit 3 }
}
"before = {0}" -f $exes['before']
"after  = {0}" -f $exes['after']
$res = @{ before = @(); after = @() }
foreach ($i in 1..$N) {
  # ABBA, NOT ABAB. A self-A/B — the same binary against a byte-identical COPY of itself,
  # md5 verified, n=6 — read the second arm as **+3.56% slower on Done with only 1 of 6
  # pairs going the other way**, plus +2.48% cpu and +1.08% wall. That is a POSITIONAL
  # bias, not drift: pairing cancels a trend between pairs and does nothing about a
  # penalty that lands on whichever run goes second (a cooler chip on the first boot of
  # each pair is the likely mechanism). Swapping the order every other pair puts each arm
  # in each position equally often, so the bias cancels inside the mean instead of being
  # attributed to the change under test.
  $order = if ($i % 2 -eq 1) { @('before', 'after') } else { @('after', 'before') }
  foreach ($side in $order) {
    robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
    $out = "bench\ab_$side.log"
    Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $exes[$side] `
          -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
          -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
          -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
    $p.ProcessorAffinity = [IntPtr]0x0FFF
    $dl = [DateTime]::Now.AddSeconds(300)
    $peak = 0
    while ([DateTime]::Now -lt $dl) {
      Start-Sleep -Milliseconds 200
      if ($p.HasExited) { break }
      try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
      if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
    }
    $sw.Stop()
    # CPU TIME, and it is the better instrument on a shared box. Wall absorbs
    # every other process on the machine — a browser stealing a core lengthens our
    # wall and leaves our CPU untouched — so the paired wall A/B here resolves
    # about +-4%, which is coarser than the changes worth making: the call
    # protocol is 225 ns over 148.9 M calls, so 10 ns/call is 1.6% of wall and
    # invisible. TotalProcessorTime measures the work THIS process did and nothing
    # else, which is what a "does this change do less work?" question actually
    # asks. Read BEFORE the kill; a killed process reports nothing.
    $cpu = 0.0
    try { $p.Refresh(); $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
    # PAGE FAULTS, read alive like the CPU time. Row 282 found the product spending LESS
    # cpu and MORE wall with rss +127 MB beside it, and "the extra wall is memory" is a
    # hypothesis until something counts it. A fault is wall the process pays and the cpu
    # clock never sees, so it is exactly the quantity that distinguishes the two.
    $faults = 0
    try { $faults = (Get-CimInstance Win32_Process -Filter "ProcessId=$($p.Id)" -ErrorAction Stop).PageFaults } catch {}
    if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
    $done = (Get-Content $out | Select-String 'Done \(([0-9.]+)s\)')
    $d = if ($done) { [double]$done.Matches[0].Groups[1].Value } else { -1 }
    # PHASE MAP. `Done (Xs)` is the guest's OWN clock and it starts at "Starting minecraft
    # server" — on this jar it covers the last ~15 s of a ~46 s run, so two thirds of the
    # boot were invisible to both numbers this harness used to print. Worse, the gap was
    # read as a post-`Done` world save, which cannot happen: the loop above KILLS the
    # process the moment `Done (` appears. The gap is startup + Bootstrap + registries +
    # recipes, and a regression that lands there needs naming.
    #
    # The log stamps whole seconds, so one run's block is coarse; over a batch it is the
    # only phase resolution available without instrumenting the guest.
    $phase = [ordered]@{ }
    $t0 = $null
    $seen = @{ }
    foreach ($line in (Get-Content $out)) {
      if ($line -notmatch '^\[(\d\d):(\d\d):(\d\d)\]') { continue }
      $sec = [int]$Matches[1] * 3600 + [int]$Matches[2] * 60 + [int]$Matches[3]
      if ($null -eq $t0) { $t0 = $sec }
      foreach ($mark in @(
          @{ k = 'env';      pat = 'Environment:' },
          @{ k = 'recipes';  pat = 'Loaded ' },
          @{ k = 'starting'; pat = 'Starting minecraft server' },
          @{ k = 'prepare';  pat = 'Preparing level' },
          @{ k = 'done';     pat = 'Done (' })) {
        if (-not $seen[$mark.k] -and $line.Contains($mark.pat)) {
          $seen[$mark.k] = $true
          $phase[$mark.k] = $sec - $t0
        }
      }
    }
    $script:walls += $sw.Elapsed.TotalSeconds
    $rss = [math]::Round($peak / 1MB)
    $res[$side] += [PSCustomObject]@{ wall = $sw.Elapsed.TotalSeconds; done = $d; rss = $rss; cpu = $cpu; phase = $phase; faults = $faults }
    $blocks = if ($phase.Count) {
      "  [boot {0}s | reg {1}s | prep {2}s]" -f $phase['env'],
        $(if ($null -ne $phase['starting'] -and $null -ne $phase['env']) { $phase['starting'] - $phase['env'] } else { '?' }),
        $(if ($null -ne $phase['done'] -and $null -ne $phase['starting']) { $phase['done'] - $phase['starting'] } else { '?' })
    } else { '' }
    "{0,-7} run {1}: wall={2,6:N2}s cpu={3,6:N2}s done={4,6:N2}s rss={5,5}MB flt={7,9}{6}" -f $side, $i, $sw.Elapsed.TotalSeconds, $cpu, $d, $rss, $blocks, $faults
  }
}
# THE BATCH JUDGES ITSELF. A fixed threshold cannot: a 42% blowout from desktop applications
# destroys a percent-level A/B exactly as thoroughly as a 3x collapse, and sails through any
# guard calibrated for the latter. Any run more than 25% above the FASTEST run saw a different
# machine than the fastest one did.
if ($script:walls.Count -ge 3) {
  $wmin = ($script:walls | Measure-Object -Minimum).Minimum
  $slow = @($script:walls | Where-Object { $_ -gt $wmin * 1.25 })
  if ($slow.Count) {
    "*** BATCH NOT LEVEL: {0} of {1} runs are >25% above the fastest ({2}s). The box changed during the batch; what follows is not only the diff." -f `
      $slow.Count, $script:walls.Count, [math]::Round($wmin, 1)
    $script:notLevel = $true
  }
}
foreach ($side in 'before', 'after') {
  $w = ($res[$side] | Measure-Object wall -Average).Average
  $mn = ($res[$side] | Measure-Object wall -Minimum).Minimum
  $dn = ($res[$side] | Measure-Object done -Average).Average
  $rs = ($res[$side] | Measure-Object rss -Maximum).Maximum
  $cw = ($res[$side] | Measure-Object cpu -Average).Average
  $cmn = ($res[$side] | Measure-Object cpu -Minimum).Minimum
  "{0,-7} mean wall={1,6:N2}s min={2,6:N2}s | mean cpu={3,7:N2}s min={4,7:N2}s | mean done={5,6:N2}s peak rss={6,5}MB n={7}" -f `
    $side, $w, $mn, $cw, $cmn, $dn, $rs, $res[$side].Count
}
# EXIT CODE, so this is scriptable. It had none, and PowerShell then leaves
# `$LASTEXITCODE` holding whatever the last NATIVE command set — which is
# `robocopy`, and robocopy answers 1 for "files copied", i.e. success. A wrapper
# that retried on a non-zero code therefore retried forever and ran 120 boots
# where it wanted 6. 0 = a usable batch, 1 = the box moved under it, 2 = an arm
# never reached `Done (` and the comparison is not a boot comparison at all.
if ($res['before'] | Where-Object { $_.done -lt 0 }) { exit 2 }
if ($res['after'] | Where-Object { $_.done -lt 0 }) { exit 2 }
if ($script:notLevel) { exit 1 }
exit 0

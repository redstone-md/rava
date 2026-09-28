# IS THE BOX QUIET ENOUGH TO MEASURE A BOOT? Dot-source this from a boot harness.
#
#   . "$PSScriptRoot\require_quiet.ps1"
#   Assert-QuietBox -Context 'boot A/B'            # refuses once (exit 4)
#   Assert-QuietBox -Context 'profile' -WarnOnly   # prints and continues
#   Wait-QuietBox   -Context 'boot A/B' -Tries 10  # waits it out, then refuses
#
# WHY THIS EXISTS. `interp_ab.ps1` refuses to run while rava/cargo/rustc are alive, and that check
# has caught real corruption. The BOOT harnesses had no such guard, and on 2026-08-13 a
# `RAVA_LAMBDA_PIC` A/B came back with three of eight runs at ~87 s against a ~61 s baseline — a
# 42% swing landing in BOTH arms, from desktop applications, not from the flag. A boot takes 60 s
# and 2 GB; anything else wanting a core during that window lands in the number.
#
# Two independent tests, because they fail differently:
#   * a FOREIGN rava/cargo/rustc — someone else's build or boot, which also fights for the port and
#     the world directory;
#   * a busy box — measured as processor-queue length and idle time, not as "which apps are open",
#     because an idle Discord costs nothing and a compiling IDE costs everything.
#
# WHY A WAIT AND NOT ONLY A REFUSAL. A refusal is not a result. A busy box is a temporary condition,
# and on 2026-08-15 an unattended confirmation run died on `queue 3.5` from the desktop's own
# applications, throwing away the whole batch. `gate_ab.ps1` had already grown its own copy of this
# loop for the same reason; it lives here now so no harness has to reimplement it a third time.

# The measurement, with no policy attached: what does the box look like right now?
function Test-QuietBox {
  param([int]$MaxQueue = 2, [int]$MinIdle = 60)
  $busy = @(Get-Process rava, cargo, rustc -ErrorAction SilentlyContinue |
            Where-Object { $_.Id -ne $PID } | Select-Object -Expand Name -Unique)
  if ($busy) {
    return [pscustomobject]@{
      Quiet = $false; Sampled = $true; Idle = $null; Queue = $null
      Reason = "{0} still running — that is another build or boot, not background noise." -f ($busy -join ', ')
    }
  }
  try {
    $idle = (Get-Counter '\Processor(_Total)\% Idle Time' -SampleInterval 1 -MaxSamples 2 -ErrorAction Stop).CounterSamples |
            Measure-Object -Property CookedValue -Average | Select-Object -Expand Average
    $queue = (Get-Counter '\System\Processor Queue Length' -SampleInterval 1 -MaxSamples 2 -ErrorAction Stop).CounterSamples |
             Measure-Object -Property CookedValue -Average | Select-Object -Expand Average
  } catch {
    # Counters unavailable is not the same as busy: proceed rather than block forever.
    return [pscustomobject]@{ Quiet = $true; Sampled = $false; Idle = $null; Queue = $null; Reason = $null }
  }
  $idle = [math]::Round($idle, 1); $queue = [math]::Round($queue, 1)
  if ($idle -lt $MinIdle -or $queue -gt $MaxQueue) {
    $top = (Get-Process | Sort-Object -Descending CPU | Select-Object -First 3 |
            ForEach-Object { $_.Name }) -join ', '
    return [pscustomobject]@{
      Quiet = $false; Sampled = $true; Idle = $idle; Queue = $queue
      Reason = "box is busy (idle {0}%, queue {1}; heaviest: {2}). A boot A/B on a loaded box measures the box." -f $idle, $queue, $top
    }
  }
  [pscustomobject]@{ Quiet = $true; Sampled = $true; Idle = $idle; Queue = $queue; Reason = $null }
}

function Assert-QuietBox {
  param(
    [string]$Context = 'measurement',
    [switch]$WarnOnly,
    # Refuse above this many runnable threads waiting for a core (2 is already contention on a
    # 12-core box when the measurement wants 1-3 of them).
    [int]$MaxQueue = 2,
    # Refuse below this % idle, sampled over ~2 s.
    [int]$MinIdle = 60
  )
  $q = Test-QuietBox -MaxQueue $MaxQueue -MinIdle $MinIdle
  if (-not $q.Quiet) {
    $msg = "REFUSING $Context : {0}" -f $q.Reason
    if ($WarnOnly) { $msg; return } else { $msg; exit 4 }
  }
  if (-not $q.Sampled) { "note: could not sample the performance counters — proceeding without the quiet check"; return }
  "quiet: idle {0}%, processor queue {1}" -f $q.Idle, $q.Queue
}

# Wait for the box to go quiet, then return. Refuses (exit 4) only after $Tries attempts, so an
# unattended run survives a desktop application that wakes up for a minute.
function Wait-QuietBox {
  param(
    [string]$Context = 'measurement',
    [int]$Tries = 10,
    [int]$SleepSeconds = 40,
    [int]$MaxQueue = 2,
    [int]$MinIdle = 60
  )
  for ($try = 1; $try -le $Tries; $try++) {
    $q = Test-QuietBox -MaxQueue $MaxQueue -MinIdle $MinIdle
    if ($q.Quiet) {
      if ($q.Sampled) { "quiet: idle {0}%, processor queue {1}" -f $q.Idle, $q.Queue }
      else { "note: could not sample the performance counters — proceeding without the quiet check" }
      return
    }
    if ($try -eq $Tries) { "REFUSING $Context after $Tries attempts : {0}" -f $q.Reason; exit 4 }
    "waiting for a quiet box ({0}/{1}): {2}" -f $try, $Tries, $q.Reason
    Start-Sleep -Seconds $SleepSeconds
  }
}

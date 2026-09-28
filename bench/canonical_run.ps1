# Canonical ChunkFill 1 64 runner: P-core affinity, High-Performance plan,
# reports SETUP_MS / FILL_MS / hash / wall / peak working set / peak private bytes.
param([int]$Runs = 1, [string]$Tag = 'canon')
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$mask = 0x0FFF   # P-cores on i5-14400F
$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null
$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'; $env:RAVA_JIT_TIER0='1'
foreach ($i in 1..$Runs) {
  # NOTE: do NOT kill every rava.exe here. This harness used to, and when a second
  # measurement (or a subagent running its own gate) was in flight, each run silently
  # killed the other's process — no output, no exit code, which reads exactly like a
  # crash and cost a real debugging detour. Each run only ever stops the pid it starts.
  $log = Join-Path $here "canon_$($Tag)_$i.log"
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath (Join-Path $here '..\target\release\rava.exe') `
        -ArgumentList @('-cp', $cp, 'ChunkFill', '1', '64') -WorkingDirectory $here `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity = [IntPtr]$mask
  # Poll peak counters while alive — the cached values are unreadable after exit.
  $peakWSb = 0; $peakPMb = 0
  while (-not $p.HasExited) {
    try {
      $p.Refresh()
      if ($p.PeakWorkingSet64 -gt $peakWSb) { $peakWSb = $p.PeakWorkingSet64 }
      if ($p.PeakPagedMemorySize64 -gt $peakPMb) { $peakPMb = $p.PeakPagedMemorySize64 }
    } catch {}
    Start-Sleep -Milliseconds 250
  }
  $p.WaitForExit(900000) | Out-Null
  $sw.Stop()
  $peakWS = [math]::Round($peakWSb / 1MB, 1)
  $peakPM = [math]::Round($peakPMb / 1MB, 1)
  $s = (Select-String $log -Pattern 'SETUP_MS=(\d+)' | Select-Object -First 1).Matches.Groups[1].Value
  $f = (Select-String $log -Pattern 'FILL_MS=(\d+)' | Select-Object -First 1).Matches.Groups[1].Value
  $h = (Select-String $log -Pattern 'ChunkFill_HASH=([0-9a-f]+)' | Select-Object -First 1).Matches.Groups[1].Value
  Write-Host ("run{0}: SETUP_MS={1} FILL_MS={2} hash={3} wall={4:N3}s peakWS={5}MiB peakPM={6}MiB" -f $i,$s,$f,$h,$sw.Elapsed.TotalSeconds,$peakWS,$peakPM)
}
powercfg /setactive $orig | Out-Null

# Real-boot A/B for RAVA_MAPKEY_SALT: interleaved OFF/ON pairs of the REAL
# `net.minecraft.server.Main --nogui` boot-to-Done from the CACHED world.
# Protocol per rava-real-boot-verdict: world restored from world_pristine before
# EVERY run; port 25599 (never the user's 25565); external wall stopwatch (the
# in-log `Done (N.Ns)` is MC's final-phase timer, not wall); stop ONLY the
# rava.exe PID we started (never java); P-core affinity + High-Performance plan.
param([int]$Pairs = 3, [int]$TimeoutSec = 600)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$mask = 0x0FFF
$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null
$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_JIT_TIER0='1'

function Invoke-BootRun([string]$tag, [bool]$salt, [int]$i) {
  # Restore the cached world byte-identically (run1 generates / run2 loads trap).
  robocopy (Join-Path $root 'world_pristine') (Join-Path $root 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  if ($salt) { $env:RAVA_MAPKEY_SALT = '1' } else { Remove-Item Env:RAVA_MAPKEY_SALT -ErrorAction SilentlyContinue }
  $log = Join-Path $here "boot_$($tag)_$i.log"
  Remove-Item $log,"$log.err" -ErrorAction SilentlyContinue
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath (Join-Path $root 'target\release\rava.exe') `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity = [IntPtr]$mask
  $peakWS = 0; $peakPM = 0; $done = $false
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    if ($p.HasExited) { break }
    try {
      $p.Refresh()
      if ($p.PeakWorkingSet64 -gt $peakWS) { $peakWS = $p.PeakWorkingSet64 }
      if ($p.PeakPagedMemorySize64 -gt $peakPM) { $peakPM = $p.PeakPagedMemorySize64 }
    } catch {}
    if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { $done = $true; break }
    if ((Test-Path "$log.err") -and (Select-String -Path "$log.err" -Pattern 'Done \(' -Quiet)) { $done = $true; break }
    Start-Sleep -Milliseconds 500
  }
  $wall = $sw.Elapsed.TotalSeconds
  # Stop ONLY the process we started (never a broad java/rava sweep).
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
  $bind = (Test-Path "$log.err") -and (Select-String -Path "$log.err" -Pattern 'FAILED TO BIND' -Quiet)
  Write-Host ("boot {0} run{1}: done={2} wall={3:N1}s peakWS={4:N1}MiB peakPM={5:N1}MiB bindfail={6}" -f `
    $tag,$i,$done,$wall,($peakWS/1MB),($peakPM/1MB),$bind)
}

foreach ($i in 1..$Pairs) {
  Invoke-BootRun 'off' $false $i
  Invoke-BootRun 'salt' $true $i
}
powercfg /setactive $orig | Out-Null

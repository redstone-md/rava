# Real-boot A/B for RAVA_BACKGROUND_JIT (wall, not Done): interleaved OFF/ON
# pairs of the REAL `net.minecraft.server.Main --nogui` boot-to-Done from the
# CACHED world. Protocol per rava-real-boot-verdict: world restored from
# world_pristine before EVERY run; external wall stopwatch to the `Done (N.Ns)`
# line; stop ONLY the rava.exe PID we started; P-core affinity + High-Perf plan.
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
. (Join-Path $here 'assert_fresh_exe.ps1')
$exe = Join-Path $root 'target\release\rava.exe'
Assert-FreshExe $exe

function Invoke-BootRun([string]$tag, [bool]$bg, [int]$i) {
  robocopy (Join-Path $root 'world_pristine') (Join-Path $root 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  if ($bg) { $env:RAVA_BACKGROUND_JIT = '1' } else { Remove-Item Env:RAVA_BACKGROUND_JIT -ErrorAction SilentlyContinue }
  $log = Join-Path $here "bgjit_$($tag)_$i.log"
  Remove-Item $log,"$log.err" -ErrorAction SilentlyContinue
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $exe `
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
  $wallDone = $sw.Elapsed.TotalSeconds
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
  $envLine = Select-String -Path $log -Pattern '^\[[0-9:.]+\] \[Server thread/INFO\] \[net.minecraft.server.Main.*\]: Environment' -Quiet
  $doneNs = $null
  $m = (Select-String -Path $log -Pattern 'Done \(([0-9.]+)s' | Select-Object -First 1)
  if ($m) { $doneNs = $m.Matches.Groups[1].Value }
  Write-Host ("bgjit {0} run{1}: done={2} doneNs={3}s wallToDone={4:N1}s peakWS={5:N1}MiB" -f `
    $tag,$i,$done,$doneNs,$wallDone,($peakWS/1MB))
}

foreach ($i in 1..$Pairs) {
  Invoke-BootRun 'off' $false $i
  Invoke-BootRun 'on' $true $i
}
powercfg /setactive $orig | Out-Null

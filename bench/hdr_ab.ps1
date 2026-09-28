# Header-layout A/B: runs ChunkFill 1 64 (or BigGraph) against the stashed
# rava_off.exe (24B header) / rava_on.exe (16B header, --features shrink-header).
# HDR is a compile-time const baked into JIT codegen, so the arms are two BINARIES,
# not an env flag. Same affinity + power plan as canonical_run.ps1.
param(
  [string]$Arm = 'off',
  [string]$Tag = 'hdr',
  [string]$Bench = 'ChunkFill',
  [string[]]$BenchArgs = @('1','64'),
  [hashtable]$Extra = @{}
)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$mask = 0x0FFF   # P-cores on i5-14400F
$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null

$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_JIT_TIER0='1'
foreach ($k in $Extra.Keys) { Set-Item "env:$k" $Extra[$k] }

$exe = Join-Path $here "..\target\release\rava_$Arm.exe"
$log = Join-Path $here "hdr_$($Tag)_$Arm.log"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$argList = @('-cp', $cp, $Bench) + $BenchArgs
$p = Start-Process -FilePath $exe -ArgumentList $argList -WorkingDirectory $here `
      -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity = [IntPtr]$mask
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
function G([string]$pat) {
  $m = Select-String $log -Pattern $pat | Select-Object -First 1
  if ($m) { $m.Matches.Groups[1].Value } else { '-' }
}
$s = G 'SETUP_MS=(\d+)'; $f = G 'FILL_MS=(\d+)'; $h = G 'ChunkFill_HASH=([0-9a-f]+)'
$sum = G 'SUM=(-?\d+)'; $nodes = G 'NODES=(\d+)'; $hot = G 'HOTNS=(\d+)'
Write-Host ("[{0}] arm={1} SETUP={2} FILL={3} hash={4} SUM={5} NODES={6} HOTNS={7} wall={8:N3}s peakWS={9}MiB peakPM={10}MiB exit={11}" -f `
  $Tag,$Arm,$s,$f,$h,$sum,$nodes,$hot,$sw.Elapsed.TotalSeconds,$peakWS,$peakPM,$p.ExitCode)
foreach ($k in $Extra.Keys) { Set-Item "env:$k" $null }
powercfg /setactive $orig | Out-Null

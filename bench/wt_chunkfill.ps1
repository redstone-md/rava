# Worktree-local ChunkFill runner: this worktree's rava.exe + this worktree's bench/out.
# Same recipe as canonical_run.ps1 (P-cores, High-Performance) but never touches the
# shared checkout, and echoes the [GC-STATS] summary lines a collector A/B needs.
#   .\wt_chunkfill.ps1 -Tag off
#   .\wt_chunkfill.ps1 -Tag on  -Envs @{RAVA_ARENA_LAMBDA='1'}
param([string]$Tag = 'run', [hashtable]$Envs = @{}, [switch]$NoPower)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe  = Join-Path $here '..\target\release\rava.exe'
$cp   = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$base = @{ RAVA_JIT='1'; RAVA_PROC_CAP_MB='4096'; RAVA_HARD_MB='3584';
           RAVA_GSON_NATIVE='1'; RAVA_M3_BITSTORAGE='1'; RAVA_JIT_TIER0='1' }
# Clear every knob this harness ever sets, so a previous run's env cannot leak in.
foreach ($k in @('RAVA_ARENA_LAMBDA','RAVA_PINNED_LAMBDA','RAVA_GC_STATS','RAVA_GC_VALIDATE',
                 'RAVA_NURSERY_KB','RAVA_CELL_CENSUS')) { Set-Item "env:$k" $null }
foreach ($k in $base.Keys) { Set-Item "env:$k" $base[$k] }
foreach ($k in $Envs.Keys) { Set-Item "env:$k" $Envs[$k] }

if (-not $NoPower) {
  $orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
  powercfg /setactive SCHEME_MIN | Out-Null
}
$log = Join-Path $here "wt_$($Tag).log"
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'ChunkFill', '1', '64') `
      -WorkingDirectory $here -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity = [IntPtr]0x0FFF
$peakWS = 0
while (-not $p.HasExited) {
  try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peakWS) { $peakWS = $p.PeakWorkingSet64 } } catch {}
  Start-Sleep -Milliseconds 250
}
$p.WaitForExit(900000) | Out-Null
$sw.Stop()
if (-not $NoPower) { powercfg /setactive $orig | Out-Null }

$g = { param($pat) $m = Select-String $log -Pattern $pat | Select-Object -First 1
       if ($m) { $m.Matches.Groups[1].Value } else { 'NONE' } }
$s = & $g 'SETUP_MS=(\d+)'; $f = & $g 'FILL_MS=(\d+)'; $h = & $g 'ChunkFill_HASH=([0-9a-f]+)'
Write-Host ("{0}: SETUP={1} FILL={2} hash={3} wall={4:N1}s peakWS={5}MiB" -f `
            $Tag, $s, $f, $h, $sw.Elapsed.TotalSeconds, [math]::Round($peakWS/1MB,1))
Select-String "$log.err" -Pattern '^\[GC-STATS\] (minor|closure)' | Select-Object -Last 2 |
  ForEach-Object { Write-Host $_.Line }

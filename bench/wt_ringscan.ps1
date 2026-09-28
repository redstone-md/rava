# Worktree-local RingScan runner — the stronghold-ring workload (findBiomeHorizontal)
# the MC boot's Done window spends 89% of its time in, standalone. Uses THIS worktree's
# rava.exe and bench/out. P-cores + High-Performance, same recipe as canonical_run.ps1.
#   .\wt_ringscan.ps1 -Tag off -Count 4
#   .\wt_ringscan.ps1 -Tag on  -Count 4 -Envs @{RAVA_PURE_MEMO='1'}
param([string]$Tag = 'run', [int]$Count = 4, [hashtable]$Envs = @{}, [switch]$NoPower)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe  = Join-Path $here '..\target\release\rava.exe'
$cp   = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$base = @{ RAVA_JIT='1'; RAVA_PROC_CAP_MB='4096'; RAVA_HARD_MB='3584';
           RAVA_GSON_NATIVE='1'; RAVA_M3_BITSTORAGE='1'; RAVA_JIT_TIER0='1' }
foreach ($k in @('RAVA_PURE_MEMO','RAVA_PURE_MEMO_PROBE','RAVA_GC_STATS')) { Set-Item "env:$k" $null }
foreach ($k in $base.Keys) { Set-Item "env:$k" $base[$k] }
foreach ($k in $Envs.Keys) { Set-Item "env:$k" $Envs[$k] }

if (-not $NoPower) {
  $orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
  powercfg /setactive SCHEME_MIN | Out-Null
}
$log = Join-Path $here "wtring_$($Tag).log"
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'RingScan', "$Count") `
      -WorkingDirectory $here -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity = [IntPtr]0x0FFF
$p.WaitForExit(1800000) | Out-Null
$sw.Stop()
if (-not $NoPower) { powercfg /setactive $orig | Out-Null }

$g = { param($pat) $m = Select-String $log -Pattern $pat | Select-Object -First 1
       if ($m) { $m.Matches.Groups[1].Value } else { 'NONE' } }
$s = & $g 'SETUP_MS=(\d+)'; $r = & $g 'RINGS_MS=(\d+)'; $h = & $g 'HASH=([0-9a-f]+)'
Write-Host ("{0}: SETUP={1} RING={2} hash={3} wall={4:N1}s" -f $Tag, $s, $r, $h, $sw.Elapsed.TotalSeconds)
Get-Content $log | Select-Object -Last 6

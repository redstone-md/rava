# Worktree-local BootPhaseC runner (block C: datapack/registry/recipe load, NO server port).
# Same env recipe as wt_chunkfill.ps1; runs from bench/ which holds world/, server.properties.
#   .\wt_bootphasec.ps1 -Tag before
param([string]$Tag = 'run', [hashtable]$Envs = @{}, [switch]$NoPower, [string]$Class = 'BootPhaseC', [string[]]$ClassArgs = @())
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe  = Join-Path $here '..\target\release\rava.exe'
$cp   = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())
$base = @{ RAVA_JIT='1'; RAVA_PROC_CAP_MB='4096'; RAVA_HARD_MB='3584';
           RAVA_GSON_NATIVE='1'; RAVA_M3_BITSTORAGE='1'; RAVA_JIT_TIER0='1' }
foreach ($k in @('RAVA_BRIDGE_STAT','RAVA_GC_STATS','RAVA_GC_VALIDATE','RAVA_NO_STR_INTRINSIC')) { Set-Item "env:$k" $null }
foreach ($k in $base.Keys) { Set-Item "env:$k" $base[$k] }
foreach ($k in $Envs.Keys) { Set-Item "env:$k" $Envs[$k] }

if (-not $NoPower) {
  $orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
  powercfg /setactive SCHEME_MIN | Out-Null
}
$log = Join-Path $here "wtc_$($Tag).log"
$sw = [Diagnostics.Stopwatch]::StartNew()
$argList = @('-cp', $cp, $Class) + $ClassArgs
$p = Start-Process -FilePath $exe -ArgumentList $argList `
      -WorkingDirectory $here -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity = [IntPtr]0x0FFF
$p.WaitForExit(1800000) | Out-Null
$sw.Stop()
if (-not $NoPower) { powercfg /setactive $orig | Out-Null }
Write-Host ("--- {0} : wall={1:N2}s exit={2}" -f $Tag, $sw.Elapsed.TotalSeconds, $p.ExitCode)
Get-Content $log | Where-Object { $_ -match '^STEP |^STRSCAN |^LEN=|recipes|^PACKS |^STEM=' }

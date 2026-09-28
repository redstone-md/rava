# Timestamped RIP + caller-attribution trace of a REAL boot, up to a log marker.
#
# WHY A SEPARATE SCRIPT. `boot_blockb_ab.ps1` measures a wall for an A/B; this one measures
# nothing — it runs ONE boot under `RAVA_PROFILE_PC_STACKS` and keeps the trace. The two must
# not be the same run: symbolisation needs the `release-diag` binary (`release` is stripped),
# and a diag binary's wall is not a number anyone should quote.
#
# The run is killed at the marker, so it never binds the port and never reaches the world —
# several of these can be in flight alongside other benchmarks.
#
#   .\pcstack_boot.ps1 -Filter 'memchr,contain,next_match' -EveryMs 10000
param(
  # Comma-separated LEAF symbol substrings to attribute. Empty = attribute every sample.
  [string]$Filter = '',
  # Dump period. A boot is a sequence of very different phases; 10 s blocks are what let a
  # dump be lined up against the guest's own log lines.
  [int]$EveryMs = 10000,
  [int]$LaneProbeMs = 0,
  [string]$Marker = 'Environment:',
  [ValidateSet('ecore','pcore')][string]$Cores = 'pcore',
  [string]$Tag = 'pcstack',
  [int]$TimeoutS = 600
)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = 'D:\code\rava'
$mask = if($Cores -eq 'ecore'){ 0xF000 } else { 0x0FFF }
$exe  = Join-Path $here '..\target\release-diag\rava.exe'
if (-not (Test-Path $exe)) {
  Write-Host "missing $exe — build it: cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile"
  exit 1
}
$cp = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())

foreach($k in @('RAVA_JIT','RAVA_JIT_TIER0','RAVA_GSON_NATIVE','RAVA_M3_BITSTORAGE')){ Set-Item "env:$k" '1' }
foreach($k in @('RAVA_RESTORE','RAVA_CHECKPOINT_FILE','RAVA_CHECKPOINT_OPS','RAVA_STACKFUL')){ Remove-Item "env:$k" -EA SilentlyContinue }
$env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_PROFILE_PC_STACKS='1'
$env:RAVA_PC_TRACE='1'
$env:RAVA_LANE_PROBE = if ($LaneProbeMs -gt 0) { "$LaneProbeMs" } else { $null }
$env:RAVA_PC_STACK_FILTER=$Filter
$env:RAVA_PROFILE_WALL_EVERY_MS="$EveryMs"

$log = Join-Path $here "$Tag.log"
if(Test-Path $log){ Remove-Item $log -Force }
if(Test-Path "$log.err"){ Remove-Item "$log.err" -Force }
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
    -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
try { $p.ProcessorAffinity=[IntPtr]$mask } catch {}
$hit=$false
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if((Test-Path $log) -and (Select-String -Path $log -Pattern $Marker -Quiet)){ $hit=$true; break }
  if($p.HasExited){ break }
  Start-Sleep -Milliseconds 250
}
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
$env:RAVA_PROFILE_PC_STACKS=$null; $env:RAVA_PC_TRACE=$null; $env:RAVA_LANE_PROBE=$null; $env:RAVA_PC_STACK_FILTER=$null; $env:RAVA_PROFILE_WALL_EVERY_MS=$null
Write-Host ("{0}: {1}s reached={2}  ->  {3}.err" -f $Tag,[math]::Round($sw.Elapsed.TotalSeconds,1),$hit,$log)

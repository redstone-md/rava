# PC profile of the INTERPRETED call microbench (JIT off).
#
# Every other profiler in this directory points at the boot. The boot's profile is dominated by
# class loading and codec decode, so it answers a different question than "where does one
# interpreted call go" — and the interpreter arc has now shipped four changes without re-taking
# the per-call profile it was planned from.
#
# Needs release-diag: `release` is stripped and symbolises to nothing (ledger row 96).
#   cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile
#
#   bench\interp_pc.ps1 -Calls 40000000 -EveryMs 1000
param(
  [string]$Class = 'CallBench2',
  [string]$Calls = '40000000',
  [int]$EveryMs = 1000,
  [string]$Tag = 'interp_pc',
  [int]$TimeoutS = 300
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$root = (Get-Location).Path
$exe = Join-Path $root 'target\release-diag\rava.exe'
if (-not (Test-Path $exe)) {
  "missing $exe — cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile"
  exit 1
}
$env:RAVA_JIT = '0'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
# PC_ALL, not the primary-target capture: with JIT off nothing ever registers a compiled range,
# so the single-thread sampler arms nothing and dumps nothing (`stamped.is_empty()`). The
# enumerating capture is the one that sees a pure interpreter.
$env:RAVA_PROFILE_PC = '1'
$env:RAVA_PROFILE_PC_ALL = '1'
$env:RAVA_PROFILE_WALL_EVERY_MS = "$EveryMs"

$log = Join-Path $PSScriptRoot "$Tag.log"
Remove-Item $log, "$log.err" -Force -EA SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -ArgumentList @('-cp', 'out', $Class, $Calls) `
      -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
try { $p.ProcessorAffinity = [IntPtr]0x0FFF; $p.PriorityClass = 'High' } catch {}
if (-not $p.WaitForExit($TimeoutS * 1000)) { $p.Kill(); $p.WaitForExit() }
foreach ($k in 'RAVA_PROFILE_PC', 'RAVA_PROFILE_PC_ALL', 'RAVA_PROFILE_WALL_EVERY_MS') { Remove-Item "env:$k" -EA SilentlyContinue }
"{0}: {1}s -> {2}.err" -f $Tag, [math]::Round($sw.Elapsed.TotalSeconds, 1), $log

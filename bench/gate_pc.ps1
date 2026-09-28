# PC profile of a GATE — the fixed-work counterpart to boot_pc.ps1.
#
# WHY A GATE AND NOT THE BOOT. A boot's profile is a profile of everything at
# once: class loading, datapack decode, worldgen and the ring, in one flat table
# whose top row is 4%. A gate runs ONE workload, its hash proves it ran the same
# one, and `bench/gate_ab.ps1` can then price a change against it at a resolution
# the boot cannot reach. Profile the thing you can measure.
#
# Needs release-diag: `release` is stripped and symbolises to nothing.
#   cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile
#
#   bench\gate_pc.ps1 -Gate chunkfill -EveryMs 1000
param(
  [ValidateSet('chunkfill', 'ringscan', 'ringlane', 'ringlanei', 'nbtsave')][string]$Gate = 'chunkfill',
  [int]$EveryMs = 1000,
  [string]$Tag = '',
  [string]$Exe = 'target\release-diag\rava.exe'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
if (-not (Test-Path $Exe)) {
  "missing $Exe — cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile"
  exit 3
}
if (-not $Tag) { $Tag = "gpc_$Gate" }
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"
$args = @{
  chunkfill = @('ChunkFill', '1', '64')
  ringscan  = @('RingScan')
  ringlane  = @('RingLane')
  ringlanei = @('RingLaneInstance')
  nbtsave   = @('NbtSave', '64', '24')
}[$Gate]
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'; $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
# PC_ALL, not the single-thread capture: the ring gates arm workers, and a
# one-thread sampler on a multi-threaded gate reports whichever thread it latched
# — the failure that once attributed a third of a boot to a parked thread.
$env:RAVA_PROFILE_PC = '1'
$env:RAVA_PROFILE_PC_ALL = '1'
$env:RAVA_PROFILE_WALL_EVERY_MS = "$EveryMs"
$log = "bench\$Tag.log"
Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $Exe -ArgumentList (@('-cp', $cp) + $args) `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log `
      -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
if (-not $p.WaitForExit(600000)) { $p.Kill(); 'TIMEOUT' }
foreach ($k in 'RAVA_PROFILE_PC', 'RAVA_PROFILE_PC_ALL', 'RAVA_PROFILE_WALL_EVERY_MS') {
  Remove-Item "env:$k" -ErrorAction SilentlyContinue
}
$h = (Get-Content $log | Select-String 'HASH=([0-9a-f]+)' | Select-Object -First 1)
"{0}: {1:N1}s hash={2} -> {3}.err" -f $Tag, $sw.Elapsed.TotalSeconds, `
  $(if ($h) { $h.Matches[0].Groups[1].Value } else { '<none>' }), $log
Get-Content $log -Tail 1

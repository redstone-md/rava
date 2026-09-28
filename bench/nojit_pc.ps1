# WHERE DOES THE INTERPRETER-ONLY BOOT GO? PC profile of `RAVA_JIT=0`, in blocks, killed on a
# deadline rather than at `Done (` — because with the JIT off this boot does not finish in any
# reasonable time.
#
# The number that motivates it: with the JIT on, block D (`Preparing level` → `Done (`) is 16 s;
# with `RAVA_JIT=0` it ran past 576 s and had not finished, against `java -Xint`'s 2 s. That is
# ~280× on the SAME work while the dispatch loop is only 6.0× — a 50-fold discrepancy that no
# per-opcode cost can explain, so the profile has to say what it actually is.
#
#   cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile
#   bench\nojit_pc.ps1 -AfterPrepS 180
param(
  [int]$EveryMs = 15000,
  # How long to keep sampling AFTER `Preparing level` before killing.
  [int]$AfterPrepS = 180,
  [string]$Tag = 'nojit_pc',
  [int]$TimeoutS = 900
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
. "$PSScriptRoot\require_quiet.ps1"
Assert-QuietBox -Context 'the no-JIT profile' -WarnOnly
$exe = 'target\release-diag\rava.exe'
if (-not (Test-Path $exe)) {
  "missing $exe — cargo build --profile release-diag -p rava-cli --features next-gen-gc,wall-profile"
  exit 1
}
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '0'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROFILE_PC = '1'; $env:RAVA_PROFILE_PC_ALL = '1'
$env:RAVA_PROFILE_WALL_EVERY_MS = "$EveryMs"
foreach ($k in 'RAVA_JIT_TIER0', 'RAVA_PC_TRACE', 'RAVA_PROFILE_PC_STACKS', 'RAVA_STACKFUL') {
  Remove-Item "env:$k" -ErrorAction SilentlyContinue
}
$log = "bench\$Tag.log"
Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
      -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$prepAt = $null
while ($sw.Elapsed.TotalSeconds -lt $TimeoutS) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if (-not $prepAt -and (Test-Path $log) -and (Select-String -Path $log -Pattern 'Preparing level' -Quiet)) {
    $prepAt = $sw.Elapsed.TotalSeconds
    "  Preparing level at t+{0}s — sampling {1}s more" -f [math]::Round($prepAt, 1), $AfterPrepS
  }
  if ($prepAt -and ($sw.Elapsed.TotalSeconds - $prepAt) -gt $AfterPrepS) { break }
}
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
foreach ($k in 'RAVA_JIT', 'RAVA_PROFILE_PC', 'RAVA_PROFILE_PC_ALL', 'RAVA_PROFILE_WALL_EVERY_MS') {
  Remove-Item "env:$k" -ErrorAction SilentlyContinue
}
"{0}: {1}s, prep at {2}s -> {3}.err" -f $Tag, [math]::Round($sw.Elapsed.TotalSeconds, 1), `
  $(if ($prepAt) { [math]::Round($prepAt, 1) } else { 'never' }), $log

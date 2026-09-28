# PGO TRAINING ON THE REAL SERVER BOOT.
#
# The shipped profile is trained on BootPhaseC + ChunkFill, chosen for one reason: both
# return from `main`. The real boot never does (docs/pgo-build.md trap 2), so the binary
# that gets measured on the boot is optimised for a PROXY of it. `RAVA_PGO_DUMP_AFTER_MS`
# (rava-cli feature `pgo-dump`) calls `__llvm_profile_write_file` mid-run and exits, which
# removes the need for the guest to cooperate at all.
#
# Requires an instrumented build carrying the feature:
#   $env:RUSTFLAGS = "-C target-cpu=native -C profile-generate=<dir>"
#   cargo build --profile release-fat -p rava-cli --features next-gen-gc,pgo-dump `
#         --target x86_64-pc-windows-msvc
# DEFAULTS ARE THE MEASURED VALUES, not estimates. 95000 was a guess sized against the
# PRODUCTION wall (~47 s) and it failed twice: the instrumented binary runs ~2x slower and
# had not reached Done at 95 s or at 110 s. 210 s reached it with room to spare. Overshooting
# only costs training time; undershooting trains on the serial prefix alone.
param(
  [string]$Exe = 'target\x86_64-pc-windows-msvc\release-fat\rava.exe',
  [int]$DumpAfterMs = 210000,
  [int]$TimeoutSec = 300,
  [string]$RunRoot = '',
  [int]$ExpectedServerPort = 25599
)
$ErrorActionPreference = 'Continue'
$repoRoot = (Resolve-Path "$PSScriptRoot\..").Path
$root = if ($RunRoot) { (Resolve-Path -LiteralPath $RunRoot -ErrorAction Stop).Path } else { $repoRoot }
$exePath = if ([IO.Path]::IsPathRooted($Exe)) { $Exe } else { Join-Path $repoRoot $Exe }

foreach ($required in 'out','world_pristine','server.properties','eula.txt','rava.toml') {
  if (-not (Test-Path -LiteralPath (Join-Path $root $required))) {
    throw "run root '$root' is missing required '$required'"
  }
}
if ($RunRoot) {
  if ($ExpectedServerPort -eq 25565) {
    throw 'isolated training must not use the interactive server port 25565'
  }
  $portLine = Get-Content -LiteralPath (Join-Path $root 'server.properties') |
              Where-Object { $_ -match '^server-port=' } | Select-Object -First 1
  if ($portLine -ne "server-port=$ExpectedServerPort") {
    throw "isolated training requires server-port=$ExpectedServerPort; found '$portLine'"
  }
  if (-not (Select-String -LiteralPath (Join-Path $root 'eula.txt') -Pattern '^eula=true$' -Quiet)) {
    throw "isolated training requires eula=true in '$root\eula.txt'"
  }
  $listener = Get-NetTCPConnection -LocalPort $ExpectedServerPort -State Listen -ErrorAction SilentlyContinue
  if ($listener) {
    throw "isolated training port $ExpectedServerPort is already in use"
  }
}

$cp = (Join-Path $root 'out') + ';' + ((Get-Content (Join-Path $repoRoot 'bench\mc-cp.txt') -Raw).Trim())
$worldPristine = Join-Path $root 'world_pristine'
$world = Join-Path $root 'world'
robocopy $worldPristine $world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null

# The instrumented binary is ~2x slower, so the dump has to land AFTER Done rather than at
# the wall the production binary would show. Overshooting only costs training time; a dump
# before Done would train on a prefix and silently skew the profile toward class loading.
$env:RAVA_JIT = '1'
$env:RAVA_NO_FAST_EXIT = '1'   # trap 1: the fast exit skips Drop and leaves a 0-byte profraw
$env:RAVA_PGO_DUMP_AFTER_MS = "$DumpAfterMs"
foreach ($key in 'RAVA_RESTORE','RAVA_CHECKPOINT_FILE','RAVA_CHECKPOINT_OPS','RAVA_STACKFUL') {
  Remove-Item "env:$key" -ErrorAction SilentlyContinue
}

$logDir = if ($RunRoot) { Join-Path $root 'bench-results' } else { Join-Path $repoRoot 'bench' }
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$out = Join-Path $logDir 'pgo_train_boot.log'
Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exePath `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory $root -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High

while (-not $p.HasExited -and $sw.Elapsed.TotalSeconds -lt $TimeoutSec) { Start-Sleep -Milliseconds 500 }
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit(); Write-Host 'TIMED OUT — no dump' }

# Reaching Done is what makes this training worth more than the proxy; say so either way
# rather than letting a prefix-only profile pass as a boot profile.
$reachedDone = (Select-String -Path $out -Pattern 'Done \(' -Quiet) -eq $true
$dumped = (Select-String -Path "$out.err" -Pattern '\[PGO-DUMP\]' -Quiet) -eq $true
Write-Host ("boot train: {0:N1}s  reachedDone={1}  dumped={2}" -f $sw.Elapsed.TotalSeconds, $reachedDone, $dumped)
if (-not $dumped) { throw 'no [PGO-DUMP] line — is the binary built with --features pgo-dump?' }
if (-not $reachedDone) { throw 'dump landed before Done — raise -DumpAfterMs' }

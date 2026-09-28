# THE PRODUCTION BUILD. Everything below is build-level only — same source, same
# features, same `target-cpu=native`, no semantic surface. Two measured wall levers
# stack here (boot-speed ledger rows 16 and 17):
#
#   --profile release-fat      wall -5.8%   4/4 pairs, zero overlap
#   + PGO (this script)        wall -15.5%  5/5 pairs, zero overlap, cpu -9.7%
#
# Product lands at target\pgo-fat\rava.exe. Verify it with bench\pgo_gates.ps1
# (ChunkFill 8790f04ace726ecf, RingScan/RingLane/RingLaneInstance 37539c6c375d0c3).
#
# Total ~13 min: instrument 4.5, train 2.5, merge, rebuild 6.
#
# `-BootTrain` trains on the REAL server boot instead of the two drivers (ledger row 104,
# measured against a same-source control: wall -3.5%, Done -6.5%, cpu -4.7% over 5 ABBA
# pairs). It costs a much longer fat LTO and DE-OPTIMISES the gate drivers -- they stay
# bit-exact but roughly halve in speed, so a boot-trained binary must not be used as a
# speed instrument for ChunkFill/RingScan work.
#
# TIMINGS, re-measured 2026-08-20 (two full runs): the 4/4 optimized build took 13m13s and
# 21m13s -- so budget for a wide range rather than the single figure this comment used to
# quote, and expect the spread to follow whatever else the box is doing.
#
# The instrumented binary is ~4.5x slower than the product, NOT ~2x: the training boot runs
# 210s against a ~47s product boot, twice, with nothing else on the machine. Reading it as 2x
# once led to a real mis-diagnosis -- a 210s training run was blamed on interference that was
# not there.
param(
  [switch]$SkipTrain,
  [switch]$BootTrain,
  [string]$RunRoot = '',
  [int]$ExpectedServerPort = 25599
)

# NOT 'Stop': cargo writes its progress to stderr, which Windows PowerShell turns into
# a terminating NativeCommandError even on a successful build. Exit codes are checked
# explicitly instead.
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$repoRoot = (Get-Location).Path
if ($BootTrain -and -not $SkipTrain -and -not $RunRoot) {
  throw 'fresh BootTrain requires an explicit isolated -RunRoot'
}
# Separate paths per training workload so switching modes never silently reuses the other
# one's profile — the two produce measurably different binaries and mixing them would be
# invisible in the output.
$profdata = Join-Path $repoRoot (if ($BootTrain) { 'target\pgo-boot.profdata' } else { 'target\pgo.profdata' })
$profdir = Join-Path $repoRoot (if ($BootTrain) { 'target\pgo-boot-data' } else { 'target\pgo-data' })
$staged = 'target\x86_64-pc-windows-msvc\release-fat\rava.exe'
$llvm = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\llvm-profdata.exe"
if (-not (Test-Path $llvm)) { throw "llvm-profdata missing — rustup component add llvm-tools-preview" }

if (-not $SkipTrain) {
  Write-Host '== 1/4 instrumented build'
  # The explicit --target keeps RUSTFLAGS off build scripts; it also disables
  # .cargo/config.toml's [build] rustflags, so target-cpu=native is repeated here.
  $env:RUSTFLAGS = "-C target-cpu=native -C profile-generate=$profdir"
  # `pgo-dump` links __llvm_profile_write_file, which only exists in an instrumented build —
  # so it is added HERE and never to the optimized build below.
  $feat = if ($BootTrain) { 'next-gen-gc,pgo-dump' } else { 'next-gen-gc' }
  cargo build --profile release-fat -p rava-cli --features $feat --target x86_64-pc-windows-msvc
  Remove-Item Env:RUSTFLAGS
  if ($LASTEXITCODE -ne 0) { throw 'instrumented build failed' }

  Write-Host '== 2/4 training runs'
  # Stale counters from a previous mode would be merged in silently.
  Remove-Item "$profdir\*.profraw" -ErrorAction SilentlyContinue
  if ($BootTrain) {
    & "$PSScriptRoot\pgo_train_boot.ps1" -Exe $staged -RunRoot $RunRoot -ExpectedServerPort $ExpectedServerPort
  } else {
    & "$PSScriptRoot\pgo_train.ps1"
  }

  Write-Host '== 3/4 merge'
  $raw = Get-ChildItem "$profdir\*.profraw"
  if (-not $raw) { throw 'no .profraw — see docs/pgo-build.md (RAVA_NO_FAST_EXIT)' }
  & $llvm merge -o $profdata $raw.FullName
  if ($LASTEXITCODE -ne 0) { throw 'llvm-profdata merge failed' }
}
if (-not (Test-Path $profdata)) { throw "no profile at $profdata — run without -SkipTrain" }

Write-Host '== 4/4 optimized build'
$env:RUSTFLAGS = "-C target-cpu=native -C profile-use=$profdata"
cargo build --profile release-fat -p rava-cli --features next-gen-gc --target x86_64-pc-windows-msvc
Remove-Item Env:RUSTFLAGS
if ($LASTEXITCODE -ne 0) { throw 'optimized build failed' }

New-Item -ItemType Directory -Force target\pgo-fat | Out-Null
Copy-Item $staged target\pgo-fat\rava.exe -Force
Get-Item target\pgo-fat\rava.exe | Select-Object FullName, Length, LastWriteTime
Write-Host 'now gate it: .\bench\pgo_gates.ps1 -Exe target\pgo-fat\rava.exe'

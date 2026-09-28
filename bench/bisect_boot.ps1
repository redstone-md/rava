# ATTRIBUTE a wall regression to the COMMIT that carries it.
#
# Builds each named ref once into `bench\bin\rava_<ref>.exe`, then runs the paired
# A/B over each adjacent pair. Every pair is a real alternating batch on the same
# restored world, so the answer is per-step and not a sum of separately-measured
# things.
#
# WHY PER-STEP AND NOT "each ref against the base": a regression that is spread
# over several commits reads as a slow climb here and as one big step there, and
# the two look identical if you only measure the ends. The steps also cost the
# same number of boots as the ends do.
#
# A refused batch (exit 4, the box got busy) is RETRIED; a non-level batch
# (exit 1) is retried too, because its own numbers say it saw two machines. An
# arm that never reached `Done (` (exit 2) is fatal for that pair and reported —
# that is a commit that does not boot, which is a finding, not a measurement.
#
#   bench\bisect_boot.ps1 -Refs 47d79ad,0c567c5,c38b520,739a7b8,HEAD -N 3
param(
  [string[]]$Refs = @(),
  [int]$N = 3,
  [int]$MaxRetry = 8
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
if (-not $Refs -or $Refs.Count -lt 2) { 'need at least two -Refs'; exit 3 }

$bin = Join-Path (Get-Location) 'bench\bin'
if (-not (Test-Path $bin)) { New-Item -ItemType Directory $bin | Out-Null }
$wt = Join-Path $env:TEMP 'rava-refactor-ab'
if (-not (Test-Path $wt)) { git worktree add $wt $Refs[0] | Out-Null }

# --- build every ref once -------------------------------------------------------
$exe = @{}
foreach ($r in $Refs) {
  $sha = (git rev-parse --short $r).Trim()
  $dst = Join-Path $bin "rava_$sha.exe"
  $exe[$r] = $dst
  if (Test-Path $dst) { "have $r ($sha)"; continue }
  "building $r ($sha)"
  Push-Location $wt
  git checkout -q $sha
  cargo build -p rava-cli --features next-gen-gc --release 2>&1 | Select-Object -Last 1
  Pop-Location
  $src = Join-Path $wt 'target\release\rava.exe'
  if (-not (Test-Path $src)) { "BUILD FAILED for $r"; exit 3 }
  Copy-Item $src $dst -Force
}

# --- A/B each adjacent pair ------------------------------------------------------
for ($i = 0; $i -lt $Refs.Count - 1; $i++) {
  $a = $Refs[$i]; $b = $Refs[$i + 1]
  "`n=== $a -> $b ==="
  for ($try = 1; $try -le $MaxRetry; $try++) {
    & "$PSScriptRoot\wt_boot_ab.ps1" -BeforeExe $exe[$a] -AfterExe $exe[$b] -N $N
    $code = $LASTEXITCODE
    if ($code -eq 0) { break }
    if ($code -eq 2) { "*** $a -> $b : an arm never reached Done — not a boot comparison"; break }
    "retry $try (exit $code)"
    Start-Sleep -Seconds 45
  }
}
exit 0

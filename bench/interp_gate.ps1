# THE INTERPRETER'S CORRECTNESS GATE — the one this tree did not have.
#
# WHY IT DID NOT EXIST (ledger row 338). `gate_ab.ps1` sets `$env:RAVA_JIT = '1'` itself, so all
# four gate hashes have only ever been produced with the compiler ON. Run any of them with
# `RAVA_JIT=0` and the process dies with **exit -1073741819 = 0xC0000005, ACCESS_VIOLATION**, about
# 35 s in, printing nothing past the log4j banner. So a change to the interpreter — which ledger
# row 337 measures as the WEAK MEMBER (compilers off: rava 1,415 s vs `java -Xint` 46.19 s = 30.6×,
# and ~33% of its time is call setup) — had no bit-exact check of any kind.
#
# WHAT THE CRASH IS (row 339). It is the GARBAGE COLLECTOR on the interpreted path: raise the
# nursery so no collection happens and the same gate completes cleanly. That is a real defect and
# it is filed as one; this script is the workaround that makes the instrument usable TODAY.
#
# WHAT IT PROVES. `ChunkFill 1 16` returns **f55ae4b5e17b36c1** interpreted AND compiled — the two
# tiers agree bit-exactly, which is what makes this a CROSS-TIER gate rather than a self-check.
# It also prices the compiler on identical fixed work: FILL_MS ~52,600 interpreted vs ~1,540
# compiled = ~34x.
#
#   bench\interp_gate.ps1              # run both tiers and compare
#   bench\interp_gate.ps1 -Interp      # interpreted only (~55 s)
param(
  [string]$Exe = 'target\release\rava.exe',
  [switch]$Interp,
  # The nursery that avoids the collector crash. Lower it to REPRODUCE the defect.
  [string]$NurseryKb = '1048576'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$want = 'f55ae4b5e17b36c1'
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
javac -nowarn -cp ((Get-Content 'bench\mc-cp.txt' -Raw).Trim()) -d out bench\ChunkFill.java 2>&1 | Out-Null

function Run-Tier([string]$jit, [string]$nursery, [string]$tag) {
  $env:RAVA_JIT = $jit
  if ($nursery) { $env:RAVA_NURSERY_KB = $nursery } else { Remove-Item env:RAVA_NURSERY_KB -EA SilentlyContinue }
  $out = "bench\interp_gate_$tag.log"
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'ChunkFill', '1', '16') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden -Wait
  $line = (Get-Content $out -Tail 1)
  $h = [regex]::Match($line, 'ChunkFill_HASH=([0-9a-f]+)').Groups[1].Value
  $ms = [regex]::Match($line, 'FILL_MS=(\d+)').Groups[1].Value
  # An ACCESS_VIOLATION is the defect this script documents, so name it rather than print a blank.
  if ($p.ExitCode -ne 0) {
    Write-Host "$tag`tEXIT=$($p.ExitCode)$(if ($p.ExitCode -eq -1073741819) { '  (0xC0000005 ACCESS_VIOLATION - the collector defect, ledger row 339)' })"
    return $null
  }
  Write-Host "$tag`tFILL_MS=$ms`tHASH=$h`t$(if ($h -eq $want) { 'OK' } else { "MISMATCH (want $want)" })"
  return $h
}

$hi = Run-Tier '0' $NurseryKb 'interp'
if (-not $Interp) { $hj = Run-Tier '1' '' 'jit' }
""
if (-not $Interp -and $hi -and $hj) {
  if ($hi -eq $hj) { "CROSS-TIER: interpreted == compiled == $hi" }
  else { "*** CROSS-TIER DIVERGENCE: interp $hi vs jit $hj ***" }
}

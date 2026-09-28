# MACHINE-PARITY GATE: does the explicit-stack machine compute what the recursive one computes?
#
# `pgo_gates.ps1` never sets RAVA_STACKFUL, so its four hashes say nothing about the trampoline
# machine — which is the machine every explicit-stack change touches. That gap was found by a
# review, not by a failure, which is the bad way to find it.
#
# The product gate cannot simply be re-run under RAVA_STACKFUL: the trampoline bypasses the
# compiled tiers (the invoke opcodes stage the callee ABOVE the tier checks), so ChunkFill 1x64
# runs fully interpreted and does not finish inside the harness timeout. So this gate uses a
# SMALLER workload and compares the two machines TO EACH OTHER rather than to a recorded
# constant: same binary, same input, hash must be identical.
#
#   bench\stackful_gate.ps1
#   bench\stackful_gate.ps1 -Height 8 -TimeoutSec 600
param(
  [string]$Exe = 'target\release\rava.exe',
  [int]$Side = 1,
  [int]$Height = 4,
  [int]$TimeoutSec = 420
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"
$env:RAVA_JIT = '1'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
"gating {0} — ChunkFill {1}x{2}, recursive vs explicit-stack" -f $Exe, $Side, $Height

$hashes = @{}
foreach ($mode in 'recursive', 'stackful') {
  if ($mode -eq 'stackful') { $env:RAVA_STACKFUL = '1' } else { Remove-Item Env:RAVA_STACKFUL -EA SilentlyContinue }
  $out = "bench\sfgate_$mode.log"
  Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'ChunkFill', "$Side", "$Height") `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out -RedirectStandardError "$out.err" `
        -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  if (-not $p.WaitForExit($TimeoutSec * 1000)) { $p.Kill(); $p.WaitForExit(); "$mode : TIMEOUT after ${TimeoutSec}s"; continue }
  $m = Select-String -Path $out -Pattern 'ChunkFill_HASH=([0-9a-f]+)'
  $h = if ($m) { $m.Matches[0].Groups[1].Value } else { '<none>' }
  $hashes[$mode] = $h
  "{0,-10} {1,7:N1}s  hash={2}" -f $mode, $sw.Elapsed.TotalSeconds, $h
}
Remove-Item Env:RAVA_STACKFUL -EA SilentlyContinue
if ($hashes.Count -eq 2) {
  if ($hashes['recursive'] -eq $hashes['stackful']) { "PARITY PASS — both machines: " + $hashes['recursive'] }
  else { "PARITY FAIL — recursive={0} stackful={1}" -f $hashes['recursive'], $hashes['stackful'] }
} else {
  "PARITY UNKNOWN — one arm did not produce a hash"
}

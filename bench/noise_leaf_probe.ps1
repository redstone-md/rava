# WHY is the ring's hottest leaf a HOST CALLBACK on every call?
#
# The lane probe's per-worker `last=site` is overwhelmingly
# `ImprovedNoise.sampleAndLerp(IIIDDDD)D`, and the freeze census classifies that site
# `unknown` — no freeze-table entry, no frozen direct target — so every evaluation
# leaves compiled code. The ring is ~80 of the boot's ~115 CPU-seconds and RingScan is
# the workload measured 61x slower than the JVM, so this one site is the largest
# single thing on the tree.
#
# Two questions, one run each: does the method COMPILE at all (RAVA_JIT_STATS), and
# what does the compiled->interpreter fallback tally say about its call sites
# (RAVA_FALLBACK_TALLY)? RingScan is used rather than the full boot: same leaf, 8x
# shorter, and it already gates bit-exact at 37539c6c375d0c3.
param([string]$Exe = 'target\release\rava.exe')
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"
$env:RAVA_JIT = '1'

foreach ($case in @(
  @{ tag = 'jitstats'; env = @{ RAVA_JIT_STATS = '1' } },
  @{ tag = 'fallback'; env = @{ RAVA_FALLBACK_TALLY = '1' } }
)) {
  Remove-Item Env:RAVA_JIT_STATS, Env:RAVA_FALLBACK_TALLY -ErrorAction SilentlyContinue
  foreach ($k in $case.env.Keys) { Set-Item -Path "Env:$k" -Value $case.env[$k] }
  $out = "bench\noiseleaf_$($case.tag).log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'RingScan') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  if (-not $p.WaitForExit(600000)) { $p.Kill(); Write-Host "$($case.tag): TIMEOUT"; continue }
  Write-Host ("== {0}  {1}" -f $case.tag, (Get-Content $out -Tail 1))
  Get-Content "$out.err" | Select-String 'sampleAndLerp|ImprovedNoise|PerlinNoise' |
    Select-Object -First 12 | ForEach-Object { $_.Line }
}
Remove-Item Env:RAVA_JIT_STATS, Env:RAVA_FALLBACK_TALLY -ErrorAction SilentlyContinue

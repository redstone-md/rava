# The SAME ring workload on the JVM, so the remaining gap is a number rather than a
# memory. RingScan gates bit-exact against the JVM already (37539c6c375d0c3), so both
# sides are provably doing identical work — which makes the ratio a clean per-op
# measurement with no "are we doing more work?" ambiguity.
#
# This decides where "much lower wall" can come from: the ring is ~80 of the boot's
# ~115 CPU-seconds, so if the ring ratio is large the ring is the whole game, and if
# it is small the serial prefix is.
param([int]$Reps = 3)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"

foreach ($side in @('jvm', 'rava')) {
  $times = @()
  for ($i = 1; $i -le $Reps; $i++) {
    $out = "bench\ringcal_${side}_$i.log"
    Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
    if ($side -eq 'jvm') {
      $p = Start-Process -FilePath 'java' -ArgumentList @('-cp', $cp, 'RingScan') `
            -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
            -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
    } else {
      $env:RAVA_JIT = '1'
      $p = Start-Process -FilePath 'target\release\rava.exe' -ArgumentList @('-cp', $cp, 'RingScan') `
            -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
            -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
    }
    $p.ProcessorAffinity = [IntPtr]0x0FFF
    $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
    if (-not $p.WaitForExit(600000)) { $p.Kill(); $times += 'TIMEOUT'; continue }
    $line = Get-Content $out -Tail 1
    if ($line -match 'RINGS_MS=(\d+).*HASH=([0-9a-f]+)') { $times += "$($Matches[1])ms/$($Matches[2])" }
    else { $times += "FAIL($line)" }
  }
  Write-Host ("{0,-5} {1}" -f $side, ($times -join '  '))
}

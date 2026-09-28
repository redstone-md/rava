# Two-binary interleaved A/B on the ring workload. Some changes have no runtime flag
# (a MemFlags choice, a build profile), so the only honest comparison is two binaries
# built from the two source states and run ABBA against each other.
# Any run whose hash is not 37539c6c375d0c3 is discarded, never timed.
param(
  [string]$AExe = 'target\base.exe',
  [string]$BExe = 'target\ro.exe',
  [int]$Pairs = 4,
  [string]$Tag = 'twoexe'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$res = @{ A = @(); B = @() }

foreach ($pair in 1..$Pairs) {
  $order = if ($pair % 2 -eq 1) { @(@('A', $AExe), @('B', $BExe)) } else { @(@('B', $BExe), @('A', $AExe)) }
  foreach ($o in $order) {
    $out = "bench\{0}_{1}{2}.log" -f $Tag, $o[0], $pair
    Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
    $p = Start-Process -FilePath $o[1] -ArgumentList @('-cp', $cp, 'RingScan') `
          -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
          -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
    $p.ProcessorAffinity = [IntPtr]0x0FFF
    $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
    if (-not $p.WaitForExit(600000)) { $p.Kill(); Write-Host "$($o[0])$pair TIMEOUT"; continue }
    $l = Get-Content $out -Tail 1
    if ($l -match 'RINGS_MS=(\d+).*HASH=([0-9a-f]+)') {
      if ($Matches[2] -ne '37539c6c375d0c3') { Write-Host "$($o[0])$pair HASH $($Matches[2])"; continue }
      $res[$o[0]] += [int]$Matches[1]
      Write-Host ("{0}{1} {2}ms" -f $o[0], $pair, $Matches[1])
    } else { Write-Host "$($o[0])$pair FAIL" }
  }
}
foreach ($k in 'A', 'B') {
  if (-not $res[$k].Count) { continue }
  # `if` is a statement in PowerShell, not an expression usable inside a -f argument.
  $exe = if ($k -eq 'A') { $AExe } else { $BExe }
  $m = $res[$k] | Measure-Object -Minimum -Average
  Write-Host ("{0} ({1}): n={2} min={3} avg={4:N0}" -f $k, $exe, $res[$k].Count, $m.Minimum, $m.Average)
}

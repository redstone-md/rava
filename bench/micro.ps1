# PINNED micro-benchmark runner.
#
# Same binary, same input, IDLE machine, unpinned: 247 / 250 / 327 / 413 / 476 / 500 ns -- a 2x
# spread, bimodal per PROCESS rather than warming up inside one. That is the E-core landing this
# project already recorded (2.1x variance; affinity + High-Perf), and every bare
# `& target\x.exe` comparison made today was missing the mask the .ps1 harnesses set.
#
#   bench\micro.ps1 -Exe "target\a.exe,target\b.exe" -Class CallBench2 -BenchArgs 4000000 -N 7
param(
  # Comma-separated: invoked via `powershell -File`, every argument arrives as one string.
  [string]$Exe,
  [string]$Class = 'CallBench2',
  [string]$BenchArgs = '4000000',
  [int]$N = 5,
  [string]$Pattern = 'RUN4_NS',
  [string]$Jit = '0'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$root = (Get-Location).Path
$env:RAVA_JIT = $Jit
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
$paths = @($Exe.Split(',') | ForEach-Object { (Resolve-Path $_.Trim()).Path })
$res = @{}
foreach ($e in $paths) { $res[$e] = @() }
$argv = @('-cp', 'out', $Class) + $BenchArgs.Split(' ')
for ($i = 1; $i -le $N; $i++) {
  $k = 0
  foreach ($e in $paths) {
    $k++
    $o = Join-Path $root "bench\micro_$k.txt"
    $r = Join-Path $root "bench\micro_$k.err"
    $p = Start-Process -FilePath $e -ArgumentList $argv -WorkingDirectory $root `
          -RedirectStandardOutput $o -RedirectStandardError $r -PassThru -WindowStyle Hidden
    if ($null -eq $p) { continue }
    $p.ProcessorAffinity = [IntPtr]0x0FFF
    $p.PriorityClass = 'High'
    if (-not $p.WaitForExit(300000)) { $p.Kill(); $p.WaitForExit(); continue }
    $m = Select-String -Path $o -Pattern "$Pattern=(.*)"
    if ($m) { $res[$e] += [double]$m.Matches[0].Groups[1].Value }
  }
}
foreach ($e in $paths) {
  $v = @($res[$e])
  if ($v.Count -eq 0) { "{0,-30} no samples" -f (Split-Path $e -Leaf); continue }
  $s = @($v | Sort-Object)
  "{0,-30} min={1,7:N1} med={2,7:N1} mean={3,7:N1} max={4,7:N1} n={5}" -f `
    (Split-Path $e -Leaf), $s[0], $s[[int]([math]::Floor($v.Count/2))],
    ($v | Measure-Object -Average).Average, $s[-1], $v.Count
}

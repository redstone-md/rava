# Are the ring's INNERMOST leaf calls served by a machine-code direct call, or do they
# fall to the general path?
#
# RAVA_JIT_STATS on RingScan: ImprovedNoise.p(I)I has run=15946 and gradDot(IDDD)D has
# run=8888, where `run` counts SLOW entries only (interpreter or jit_call's synthetic
# frame) — a machine-code PIC/deep/direct hit never increments it. Both are PRIVATE, so
# they are invokespecial sites resolved by jit_direct_resolve rather than the PIC.
#
# RAVA_JIT_DIRECT_PROBE classifies every static/special site: 2 = LEAF+compiled (a
# no-rooting direct call could serve it), 1 = compiled but NON-leaf (a rooted direct
# call could), 0 = floor (uncompiled/native/not warmed → stays on rava_invoke).
param([string]$Exe = 'target\release\rava.exe')
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_JIT_DIRECT_PROBE = '1'
$out = 'bench\directsite.log'
Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'RingScan') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
if (-not $p.WaitForExit(600000)) { $p.Kill(); Write-Host 'TIMEOUT'; exit 1 }
Remove-Item Env:RAVA_JIT_DIRECT_PROBE -ErrorAction SilentlyContinue
Write-Host (Get-Content $out -Tail 1)
Get-Content "$out.err" | Select-String 'DIRECT|direct' | Select-Object -Last 25 | ForEach-Object { $_.Line }

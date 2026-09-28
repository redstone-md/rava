# What does mimalloc actually see and accept? rava-cli links mimalloc, and the RIP
# profile puts the allocator at ~11.7% of serial self-time — but every mimalloc knob
# is a no-op if the option name is wrong for the linked version or if the OS refuses
# (large pages need SeLockMemoryPrivilege on Windows). MIMALLOC_VERBOSE prints the
# recognized options and the page/arena decisions at init, so this settles which
# knobs are even available before any A/B spends boots on them.
param([int]$Seconds = 25)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:MIMALLOC_VERBOSE = '1'
$env:MIMALLOC_SHOW_STATS = '1'
$out = 'bench\mi_probe.log'
Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath 'target\pgo-fat\rava.exe' `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
Start-Sleep -Seconds $Seconds
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
Remove-Item Env:MIMALLOC_VERBOSE, Env:MIMALLOC_SHOW_STATS
Get-Content "$out.err" | Select-String 'mimalloc|mi_' | Select-Object -First 40

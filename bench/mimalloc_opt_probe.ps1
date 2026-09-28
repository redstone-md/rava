# Which mimalloc knobs actually ENGAGE here (libmimalloc-sys 0.1.49 = mimalloc v2.2)?
# Large/huge OS pages need SeLockMemoryPrivilege on Windows and silently fall back
# without it, so acceptance has to be observed, not assumed. Verbose init tells us
# before any A/B spends boots.
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'

foreach ($case in @(
  @{ tag = 'large';  env = @{ MIMALLOC_LARGE_OS_PAGES = '1' } },
  @{ tag = 'huge';   env = @{ MIMALLOC_RESERVE_HUGE_OS_PAGES = '2' } },
  @{ tag = 'nopurge'; env = @{ MIMALLOC_PURGE_DELAY = '-1' } }
)) {
  Remove-Item Env:MIMALLOC_LARGE_OS_PAGES, Env:MIMALLOC_RESERVE_HUGE_OS_PAGES, `
              Env:MIMALLOC_PURGE_DELAY -ErrorAction SilentlyContinue
  foreach ($k in $case.env.Keys) { Set-Item -Path "Env:$k" -Value $case.env[$k] }
  $env:MIMALLOC_VERBOSE = '1'
  $out = "bench\mi_$($case.tag).log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath 'target\pgo-fat\rava.exe' `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  Start-Sleep -Seconds 20
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  Write-Host ("== {0}" -f $case.tag)
  Get-Content "$out.err" | Select-String 'mimalloc' | Select-Object -First 8
}
Remove-Item Env:MIMALLOC_VERBOSE, Env:MIMALLOC_LARGE_OS_PAGES, `
            Env:MIMALLOC_RESERVE_HUGE_OS_PAGES, Env:MIMALLOC_PURGE_DELAY -ErrorAction SilentlyContinue

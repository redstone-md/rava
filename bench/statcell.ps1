# RACE OR CODEGEN? Depth 32 crashes the boot in 2.1s. Turning the BACKGROUND compiler off makes
# compilation synchronous: the same methods compile, but nothing is concurrent. If it still
# crashes, the bug is the emitted code; if it boots, the bug is in the background pipeline.
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
# HYPOTHESIS UNDER TEST: `reserved_static_slots` is reserved on the VM thread BEFORE a job is
# queued and freed LIFO. The synchronous site says so in as many words -- "reserved contiguously
# by this compile (single-threaded path), so one LIFO free unwinds both" -- and that assumption
# dies with several jobs in flight: one job's failure LIFO-frees another job's cells.
foreach ($cfg in @(
    @{ bg = '1'; nc = '0'; t = 'background d32, static cells ON' },
    @{ bg = '1'; nc = '1'; t = 'background d32, static cells OFF' })) {
  $env:RAVA_BACKGROUND_JIT = $cfg.bg
  $env:RAVA_BACKGROUND_JIT_QUEUE = '32'
  if ($cfg.nc -eq '1') { $env:RAVA_NO_REF_STATIC_CELLS = '1' }
  else { [Environment]::SetEnvironmentVariable('RAVA_NO_REF_STATIC_CELLS', $null) }
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $o = 'bench\syncjit.log'
  Remove-Item -LiteralPath $o, "$o.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath 'target\pgo-fat\rava.exe' `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $o `
        -RedirectStandardError "$o.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(200)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 200
    if ($p.HasExited) { break }
    if (Test-Path $o) { if ((Get-Content $o -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $dn = (Get-Content $o | Select-String 'Done \(([0-9.]+)s\)')
  $v = if ($dn) { 'BOOTED Done=' + $dn.Matches[0].Groups[1].Value } else { 'FAILED' }
  "{0,-30} {1,6:N1}s {2}" -f $cfg.t, $sw.Elapsed.TotalSeconds, $v
}
foreach ($ev in 'RAVA_BACKGROUND_JIT', 'RAVA_BACKGROUND_JIT_QUEUE', 'RAVA_NO_REF_STATIC_CELLS') {
  [Environment]::SetEnvironmentVariable($ev, $null)
}

# The rava half of the JVM mode table (ledger row 182). JVM: -Xint 47.89s, C1-only 5.74s.
# rava's JIT-on boot is 45.7s -- level with HotSpot's INTERPRETER. So the question is what
# rava's own JIT is worth, measured the same way.
param([int]$N = 1)
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
foreach ($jit in @('0','1')) {
  $env:RAVA_JIT = $jit
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\ravajit_$jit.log"
  Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath 'target\pgo-fat\rava.exe' -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(900)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 400
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  $cpu = 0.0; try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $d = (Get-Content $out | Select-String 'Done \(([0-9.]+)s\)')
  $done = if ($d) { $d.Matches[0].Groups[1].Value } else { 'TIMEOUT' }
  "RAVA_JIT={0}  wall={1,7:N2}s cpu={2,7:N1}s Done={3}" -f $jit, $sw.Elapsed.TotalSeconds, $cpu, $done
}

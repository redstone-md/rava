# HOW MUCH OF THE JVM'S 6.6 s IS ITS JIT, AND HOW MUCH IS ITS RUNTIME DESIGN?
#
# -Xint            : interpreter only, no compilation at all
# -TieredStopAtLevel=1 : C1 only (fast, unoptimised code, no profiling/C2)
# default          : full tiered C1+C2
#
# rava's own boot is 45.7 s with its JIT on. If -Xint lands anywhere near that, the gap is the
# COMPILER; if -Xint is far slower, the gap is the runtime and the compiler is not the story.
param([string[]]$Modes = @('-Xint', '-XX:TieredStopAtLevel=1', ''))
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
foreach ($m in $Modes) {
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $tag = if ($m -eq '') { 'default' } else { $m -replace '[^A-Za-z0-9]','' }
  $out = "bench\jvmmode_$tag.log"
  Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
  $argv = @()
  if ($m -ne '') { $argv += $m }
  $argv += @('-cp', $cp, 'net.minecraft.server.Main', '--nogui')
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath 'java' -ArgumentList $argv `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(600)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 250
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  $cpu = 0.0; try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $d = (Get-Content $out | Select-String 'Done \(([0-9.]+)s\)')
  $done = if ($d) { $d.Matches[0].Groups[1].Value } else { 'TIMEOUT' }
  $label = if ($m -eq '') { 'default (C1+C2)' } else { $m }
  "{0,-24} wall={1,7:N2}s cpu={2,7:N1}s Done={3}" -f $label, $sw.Elapsed.TotalSeconds, $cpu, $done
}

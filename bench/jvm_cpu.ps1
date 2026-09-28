# HOW MANY CORES DOES THE REFERENCE ENGINE USE? — the measurement that decides whether rava's
# gap is per-core efficiency or parallelism.
#
# Row 440 measured rava at cpu/wall = 1.04 and concluded the goal needs parallel bytecode. That
# conclusion is only sound if `java` reaches its 8 s by using MORE cores. Nobody had measured it.
param([int]$TimeoutS = 300, [switch]$Rava, [switch]$NoJit, [string[]]$JavaOpts = @(),
      [string]$Exe = 'target
elease
ava.exe')  # so two binaries can be interleaved  # -NoJit = the STANDING benchmark:
                                                            # `java -Xint` against `RAVA_JIT=0`
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
& robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$log = 'bench\jvmcpu.log'
Remove-Item $log, "$log.err" -EA SilentlyContinue
if ($Rava) {
  if ($NoJit) {
    $env:RAVA_JIT = '0'
    Remove-Item env:RAVA_JIT_TIER0, env:RAVA_OFFLOAD_FIRST -EA SilentlyContinue
  } else {
    # ⚠**DO NOT force `RAVA_OFFLOAD_FIRST` here.** It is opt-in (rows 432, 451) and worth 2.13x,
    # so forcing it made every "product" number this harness printed an OFFLOAD-FIRST number —
    # and an A/B of anything that interacts with the lane then measured on top of it, which is
    # how a 51.8 s reading looked like a new win when it was the flag's own known figure.
    # Pass it in the environment when that is the arm being measured.
    $env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'
  }
  $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
  $file = Join-Path (Get-Location) 'target\release\rava.exe'
  $argv = @('-cp', $cp, 'net.minecraft.server.Main', '--nogui')
} else {
  $file = 'java'
  $argv = if ($NoJit) { @('-Xint', '-cp', $cp, 'net.minecraft.server.Main', '--nogui') }
          else { @($JavaOpts) + @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') }
  # -JavaOpts lets one question be asked of the reference engine directly: e.g.
  # `-JavaOpts '-Dmax.bg.threads=1'` caps Minecraft's own background executor at one thread, which
  # separates GUEST parallelism (the reload's worker pool) from the JVM's compiler threads in the
  # cores column -- the two things a bare `cores=3.2` cannot tell apart.
}
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $file -ArgumentList $argv -WorkingDirectory (Get-Location) `
      -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
$null = $p.Handle
try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
$dl = [DateTime]::Now.AddSeconds($TimeoutS)
while ([DateTime]::Now -lt $dl -and -not $p.HasExited) {
  Start-Sleep -Milliseconds 250
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { break }
}
$sw.Stop()
# Read CPU BEFORE the kill — unavailable afterwards.
$cpu = try { $p.Refresh(); $p.TotalProcessorTime.TotalSeconds } catch { 0 }
$peak = try { [int]($p.PeakWorkingSet64 / 1MB) } catch { 0 }
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
$m = Select-String -Path $log -Pattern 'Done \(([\d.]+)s\)' | Select-Object -Last 1
$done = if ($m) { $m.Matches.Groups[1].Value } else { 'NEVER' }
Write-Host ("{0}: wall={1:N1}s cpu={2:N1}s cores={3:N2} Done={4} peakRSS={5}MB" -f `
  $(if ($Rava) { if ($NoJit) { 'rava JIT=0' } else { 'rava' } } elseif ($NoJit) { 'java -Xint' } else { 'java' }), $sw.Elapsed.TotalSeconds, $cpu,
  ($cpu / [math]::Max($sw.Elapsed.TotalSeconds, 0.001)), $done, $peak)

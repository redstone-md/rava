# RAVA_JIT_LOOP_EAGER on the real boot.
#
# RAVA has no OSR, so a method entered ONCE that contains a back edge interprets its whole
# loop even after its compiled body exists. Measured in isolation (bench/CallBench2.java):
# first entry 364.0 ns/call, re-entry 0.6 ns/call -- a 600x cliff, and 0.6 ns is level with
# the JVM's 0.43-0.58. The compiled code is fine; the FIRST ENTRY is the whole cost.
#
# This flag compiles such a method on its first call instead. Default OFF, and it additionally
# excludes <clinit> and methods over 4096 bytes -- which is most of what an MC boot is, so a
# neutral result here would NOT clear the idea, only this flag's admission.
param([int]$Pairs = 2)
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$exe = 'target\pgo-fat\rava.exe'
$env:RAVA_JIT = '1'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
function Run([string]$eager, [string]$tag) {
  if ($eager -eq '1') { $env:RAVA_JIT_LOOP_EAGER = '1' }
  else { [Environment]::SetEnvironmentVariable('RAVA_JIT_LOOP_EAGER', $null) }
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\loopeager_$tag.log"
  Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(240)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 250
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  $cpu = 0.0; try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $d = (Get-Content $out | Select-String 'Done \(([0-9.]+)s\)')
  $done = if ($d) { [double]$d.Matches[0].Groups[1].Value } else { -1 }
  [pscustomobject]@{ tag = $tag; wall = [math]::Round($sw.Elapsed.TotalSeconds,2); done = $done; cpu = [math]::Round($cpu,1) }
}
$rows = @()
for ($i = 1; $i -le $Pairs; $i++) {
  $rows += Run '0' "B$i"
  $rows += Run '1' "A$i"
  $rows += Run '1' "A${i}b"
  $rows += Run '0' "B${i}b"
}
$rows | Format-Table -AutoSize
$B = $rows | Where-Object { $_.tag -like 'B*' }
$A = $rows | Where-Object { $_.tag -like 'A*' }
"OFF  wall={0:N2}  done={1:N2}  cpu={2:N1}" -f (($B|Measure-Object wall -Average).Average), (($B|Measure-Object done -Average).Average), (($B|Measure-Object cpu -Average).Average)
"ON   wall={0:N2}  done={1:N2}  cpu={2:N1}" -f (($A|Measure-Object wall -Average).Average), (($A|Measure-Object done -Average).Average), (($A|Measure-Object cpu -Average).Average)
"wall min/max OFF {0:N2}/{1:N2}   ON {2:N2}/{3:N2}" -f (($B|Measure-Object wall -Minimum).Minimum), (($B|Measure-Object wall -Maximum).Maximum), (($A|Measure-Object wall -Minimum).Minimum), (($A|Measure-Object wall -Maximum).Maximum)

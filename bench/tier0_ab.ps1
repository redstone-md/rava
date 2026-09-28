# RAVA_JIT_TIER0: eager-compile once-entered loop methods INCLUDING <clinit> and
# methods over 4096 bytes -- the two exclusions that make the narrower flag neutral on a boot.
# With no OSR those methods interpret their whole loop while a compiled body sits unused
# (364 ns/call first entry vs 0.6 ns re-entered, ledger row 171), and an MC boot is mostly
# long class initializers. Gates must be run separately: the tree records a latent miscompile
# once exposed in Blocks.<clinit> by all-loop eager.
param([int]$Pairs = 2)
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$exe = 'target\eagerall.exe'
$env:RAVA_JIT = '1'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
function Run([string]$eager, [string]$tag) {
  if ($eager -eq '1') { $env:RAVA_JIT_TIER0 = '1' }
  else { [Environment]::SetEnvironmentVariable('RAVA_JIT_TIER0', $null) }
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\tier0_$tag.log"
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

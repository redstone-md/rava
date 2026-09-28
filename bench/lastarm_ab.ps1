# Guest map `index` moved from std HashMap (SipHash/RandomState) to FastMap (FxHasher).
# Caller-attributed PC sampling put memchr at 3-6% of the serial prefix and named string-keyed
# map lookups as its callers, with HashMap::get<MapKey,usize,RandomState> on the stack.
# Sound because `index` is a lookup accelerator only -- guest iteration order comes from
# `entries`, and every traversal of `index` is order-independent. ABBA, fresh process, pinned.
param([int]$Pairs = 3)
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
function Run([string]$exe, [string]$tag) {
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\lastarm_$tag.log"
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
  $rows += Run 'target\lastarm_before.exe' "B$i"
  $rows += Run 'target\lastarm_after.exe'  "A$i"
  $rows += Run 'target\lastarm_after.exe'  "A${i}b"
  $rows += Run 'target\lastarm_before.exe' "B${i}b"
}
$rows | Format-Table -AutoSize
$B = $rows | Where-Object { $_.tag -like 'B*' }
$A = $rows | Where-Object { $_.tag -like 'A*' }
"BEFORE  wall={0:N2}  done={1:N2}  cpu={2:N1}   n={3}" -f (($B|Measure-Object wall -Average).Average), (($B|Measure-Object done -Average).Average), (($B|Measure-Object cpu -Average).Average), $B.Count
"AFTER   wall={0:N2}  done={1:N2}  cpu={2:N1}   n={3}" -f (($A|Measure-Object wall -Average).Average), (($A|Measure-Object done -Average).Average), (($A|Measure-Object cpu -Average).Average), $A.Count
"min/max BEFORE wall {0:N2}/{1:N2}   AFTER wall {2:N2}/{3:N2}" -f (($B|Measure-Object wall -Minimum).Minimum), (($B|Measure-Object wall -Maximum).Maximum), (($A|Measure-Object wall -Minimum).Minimum), (($A|Measure-Object wall -Maximum).Maximum)

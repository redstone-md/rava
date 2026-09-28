# Does the bench's own P-core pin COST Done? Interleaved ABBA, A = pinned 0x0FFF (what every
# other bench in this tree does), B = no affinity mask at all (what a real run gets).
#
# Why it can matter: `available_parallelism` honours the process affinity mask on Windows, so
# pinning to the 6 P-cores' 12 hyperthreads sizes every worker pool at 11 instead of 15 and
# hides the 4 E-cores. The Done window is ring-parallel, so that is exactly where it would show.
#
# The pin exists for NOISE control, so an unpinned arm is expected to be noisier — that is the
# cost of the answer, not a flaw in it. External CPU-seconds are recorded per run so a polluted
# sample is visible rather than averaged in silently.
param([int]$Pairs = 2, [string]$Exe = 'target\pgo-boot\rava.exe', [int]$TimeoutSec = 200)

$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

function Invoke-Boot {
  param([string]$Arm, [int]$Idx, [bool]$Pin)
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $env:RAVA_JIT = '1'
  $out = "bench\aff_${Arm}$Idx.log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue

  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  if ($Pin) { $p.ProcessorAffinity = [IntPtr]0x0FFF }
  $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
  $cpuAll0 = ((Get-Process).CPU | Measure-Object -Sum).Sum

  $wall = $null
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    Start-Sleep -Milliseconds 250
    if (Test-Path $out) { if ((Get-Content $out -Tail 3 -ErrorAction SilentlyContinue) -match 'Done \(') { $wall = $sw.Elapsed.TotalSeconds; break } }
    if ($p.HasExited) { break }
  }
  $cpu = $null
  if (-not $p.HasExited) { try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}; $p.Kill(); $p.WaitForExit() }
  $ext = [math]::Round(((Get-Process).CPU | Measure-Object -Sum).Sum - $cpuAll0 - $(if ($cpu) { $cpu } else { 0 }), 1)

  $done = $null
  $m = Get-Content $out -ErrorAction SilentlyContinue | Select-String 'Done \(([\d.]+)s\)' | Select-Object -First 1
  if ($m) { $done = [double]$m.Matches[0].Groups[1].Value }
  [pscustomobject]@{ Arm = $Arm; Idx = $Idx; Valid = ($null -ne $wall); Wall = $wall; Done = $done; Cpu = $cpu; ExtCpu = $ext }
}

$rows = @()
for ($i = 1; $i -le $Pairs; $i++) {
  $order = if ($i % 2 -eq 1) { @(@('A', $true), @('B', $false)) } else { @(@('B', $false), @('A', $true)) }
  foreach ($o in $order) {
    $r = Invoke-Boot -Arm $o[0] -Idx $i -Pin $o[1]
    $rows += $r
    if ($r.Valid) { Write-Host ("{0}{1}  wall={2,6:N2}s  done={3,7}  cpu={4,7:N1}  extcpu={5}" -f $r.Arm, $r.Idx, $r.Wall, $r.Done, $r.Cpu, $r.ExtCpu) }
    else { Write-Host ("{0}{1}  INVALID (no Done)" -f $r.Arm, $r.Idx) }
  }
}
foreach ($arm in 'A', 'B') {
  $v = $rows | Where-Object { $_.Arm -eq $arm -and $_.Valid }
  if ($v) {
    $w = ($v.Wall | Measure-Object -Average -Minimum -Maximum); $d = ($v.Done | Measure-Object -Average)
    Write-Host ("{0} ({1}): n={2}  wall avg={3:N2} min={4:N2} max={5:N2}  done avg={6:N2}" -f `
      $arm, $(if ($arm -eq 'A') { 'pinned 0x0FFF' } else { 'unpinned' }), $v.Count, $w.Average, $w.Minimum, $w.Maximum, $d.Average)
  } else { Write-Host "$arm : NO VALID RUNS" }
}

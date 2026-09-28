# A/B ANY env flag on a SHORT, DETERMINISTIC, allocation-heavy workload, with the bit-exact
# gate hash printed for every single run.
#
# ⚠A 50-second boot cannot be A/B'd on this box: a null test against a flag that does not exist
# read min +5.06% / median +10.30%. ChunkFill runs in ~36 CPU-seconds, so 8 rounds fit in the
# time one boot A/B takes, and it is the WORLDGEN workload — the allocation-heaviest thing the
# engine does. It also reports PEAK RSS per run, so a memory/speed trade is settled in the same
# pass.
#
# ⭐Its first use refuted a boot A/B of its own: `MIMALLOC_PURGE_DELAY=500` read -4.32% on the
# boot and **-0.01% here over 8 rounds**, which is exactly what the +10.30% null test predicted.
#
# `-CpPrefix` A/Bs a CLASSPATH ENTRY instead of an env flag — the ON arm puts it ahead of the
# normal classpath, the OFF arm does not. That is how a guest-bytecode override of a natively
# served `java.*` class is priced: one binary, one workload, one classpath entry of difference.
param([int]$Rounds = 8, [string]$Chunks = '64', [string]$Flag = 'MIMALLOC_PURGE_DELAY',
      [string]$On = '500', [string]$Off = '0', [string]$CpPrefix = '')
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$exe = Join-Path (Get-Location) 'target\release\rava.exe'
function Once([string]$delay) {
  $env:RAVA_JIT='1'; $env:RAVA_JIT_TIER0='1'
  $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
  $cpUse = $cp
  if ($CpPrefix -ne '') {
    if ($delay -eq $On) { $cpUse = $CpPrefix + ';' + $cp }
  } else {
    if ($delay -eq '') { Remove-Item "env:$Flag" -EA SilentlyContinue } else { Set-Item "env:$Flag" $delay }
  }
  $log = 'bench\purgeab.log'
  Remove-Item $log,"$log.err" -EA SilentlyContinue
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp',$cpUse,'ChunkFill','1',$Chunks) `
       -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
       -PassThru -WindowStyle Hidden
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $peak = 0
  while (-not $p.HasExited) {
    Start-Sleep -Milliseconds 100
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch { break }
  }
  $p.WaitForExit()
  try { if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
  $hash = ''
  if (Test-Path $log) { $m = Select-String -Path $log -Pattern '([0-9a-f]{16})' | Select-Object -First 1; if ($m) { $hash = $m.Matches[0].Value } }
  [pscustomobject]@{ cpu = [math]::Round($p.TotalProcessorTime.TotalSeconds,2); rss = [int]($peak/1MB); hash = $hash }
}
$null = Once $Off   # discarded warm-up
$a = @(); $b = @()
for ($i = 1; $i -le $Rounds; $i++) {
  if ($i % 2 -eq 1) { $x = Once $Off; $y = Once $On } else { $y = Once $On; $x = Once $Off }
  $a += $x; $b += $y
  Write-Host ("  r{0}: OFF cpu={1} rss={2}MB {3} | ON cpu={4} rss={5}MB {6}" -f $i,$x.cpu,$x.rss,$x.hash,$y.cpu,$y.rss,$y.hash)
}
function Med($v) { $s = $v | Sort-Object; if ($s.Count % 2) { $s[[int](($s.Count-1)/2)] } else { ($s[$s.Count/2-1] + $s[$s.Count/2]) / 2 } }
$a0 = $a | ForEach-Object { $_.cpu }; $b0 = $b | ForEach-Object { $_.cpu }
$am = Med $a0; $bm = Med $b0
Write-Host ("CPU-s  OFF min={0} median={1} | ON min={2} median={3} | delta_median={4:N2}%  (negative = ON faster)" -f ($a0|Measure-Object -Minimum).Minimum,$am,($b0|Measure-Object -Minimum).Minimum,$bm, (($bm-$am)*100/$am))
Write-Host ("RSS-MB OFF max={0} | ON max={1}" -f (($a|ForEach-Object{$_.rss})|Measure-Object -Maximum).Maximum, (($b|ForEach-Object{$_.rss})|Measure-Object -Maximum).Maximum)
Write-Host ("hashes OFF: {0}" -f (($a|ForEach-Object{$_.hash}) -join ','))
Write-Host ("hashes ON: {0}" -f (($b|ForEach-Object{$_.hash}) -join ','))

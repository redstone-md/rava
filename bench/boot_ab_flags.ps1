# Interleaved same-binary boot A/B for a SET of flags at once, with the three mitigations the
# null test forced: a discarded warm-up boot, alternating order, and a median beside the min.
# Read `boot_ab_flag.ps1`'s header first — this box has returned 7%+ "disjoint" between
# identical arms, and only a fresh null test says whether a small result here means anything.
param([string[]]$Flags, [int]$Rounds = 4, [int]$TimeoutS = 400, [int]$MaxRetry = 4)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$script:seq = 0
# Retry wrapper: keep booting this arm until one completes, so a faulted boot costs time and not
# a contaminated sample. Gives up after $MaxRetry and lets the caller see the gap.
function BootArm([bool]$set) {
  for ($t = 0; $t -lt $MaxRetry; $t++) {
    $r = BootOnce $set
    if ($r) { return $r }
  }
  return $null
}

function BootOnce([bool]$set) {
  $script:seq++
  & robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  $env:RAVA_JIT='1'; $env:RAVA_JIT_TIER0='1'; $env:RAVA_OFFLOAD_FIRST='1'
  $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
  $env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'
  foreach ($f in $Flags) { if ($set) { Set-Item "env:$f" '1' } else { Remove-Item "env:$f" -EA SilentlyContinue } }
  $log='bench\abflags.log'; Remove-Item $log,"$log.err" -EA SilentlyContinue
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath 'target\release\rava.exe' -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity=[IntPtr]0x0FFF
  $dl=[DateTime]::Now.AddSeconds($TimeoutS)
  while([DateTime]::Now -lt $dl){ Start-Sleep -Milliseconds 250; if($p.HasExited){break}; if(Test-Path $log){ if(Select-String -Path $log -Pattern 'Done \(([0-9.]+)s\)' -Quiet){break} } }
  # WAIT FOR THE WHOLE LINE, NOT ITS PREFIX. Breaking on 'Done \(' raced the flush: the very
  # next Select-String for the full pattern could read a half-written line, so the row scored
  # Done=0 AND stopped the clock early -- a row biased FAST, landing in whichever arm was
  # unlucky. Both mitigations in one: the wait and the parse now use the same pattern.
  $sw.Stop(); $cpu=0.0; try{$cpu=$p.TotalProcessorTime.TotalSeconds}catch{}
  $d=0.0; $m=Select-String -Path $log -Pattern 'Done \(([0-9.]+)s\)'; if($m){$d=[double]$m.Matches[0].Groups[1].Value}
  if(-not $p.HasExited){$p.Kill();$p.WaitForExit()}
  # A BOOT THAT NEVER PRINTED `Done` IS NOT A DATA POINT. The engine faults on a large share of
  # boots (0xC0000005 in the direct-call probe), and a faulted boot exits EARLY -- so it scores a
  # SHORT wall and a SMALL CPU and then lands in whichever arm was unlucky, which is exactly how a
  # dead boot reads as a win. Retry up to `$MaxRetry` times, and if the arm cannot produce a
  # completed boot, return $null so the caller drops it rather than averaging a corpse.
  if ($d -eq 0.0) {
    Write-Host ("    (boot {0} produced no Done -- faulted or timed out; discarded)" -f $script:seq)
    return $null
  }
  [pscustomobject]@{ Wall=[math]::Round($sw.Elapsed.TotalSeconds,1); Cpu=[math]::Round($cpu,1); Done=$d; Seq=$script:seq }
}
Write-Host "  warm-up boot (discarded)..."
$null = BootArm $false
$on=@(); $off=@()
foreach ($r in 1..$Rounds) {
  if ($r % 2 -eq 1) { $o = BootArm $false; $n = BootArm $true }
  else              { $n = BootArm $true;  $o = BootArm $false }
  if ($o) { $off += $o }; if ($n) { $on += $n }
  if (-not $o -or -not $n) { Write-Host "  round $r : an arm never completed a boot -- round dropped"; continue }
  Write-Host ("  round {0}: OFF wall={1} cpu={2} Done={3}   ON wall={4} cpu={5} Done={6}" -f `
    $r, $off[-1].Wall, $off[-1].Cpu, $off[-1].Done, $on[-1].Wall, $on[-1].Cpu, $on[-1].Done)
}
function Median($xs) { $s=$xs|Sort-Object; $n=$s.Count; if($n -eq 0){return 0}; if($n%2){$s[[int](($n-1)/2)]}else{($s[$n/2-1]+$s[$n/2])/2} }
$offCpu=$off|ForEach-Object{$_.Cpu}; $onCpu=$on|ForEach-Object{$_.Cpu}
$mo=($offCpu|Measure-Object -Minimum).Minimum; $mn=($onCpu|Measure-Object -Minimum).Minimum
Write-Host ("CPU-s: ON min={0} OFF min={1} delta={2:P2} | ON median={3} OFF median={4} delta={5:P2}" -f `
  $mn,$mo,(($mn-$mo)/$mo),(Median $onCpu),(Median $offCpu),(((Median $onCpu)-(Median $offCpu))/(Median $offCpu)))
Write-Host ("order: ON {0}  OFF {1}" -f (($on|ForEach-Object{$_.Seq}) -join '/'),(($off|ForEach-Object{$_.Seq}) -join '/'))
Write-Host ("wall: ON {0}  OFF {1}" -f (($on|ForEach-Object{$_.Wall}) -join '/'),(($off|ForEach-Object{$_.Wall}) -join '/'))

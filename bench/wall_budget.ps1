# WHERE IS THE WALL, AND HOW MUCH OF IT IS SINGLE-THREADED?
#
# Written after three consecutive levers whose MECHANISM was right and whose PRICE was wrong
# (the jit_run cell realloc, the instanceof memo on Done, and the direct-mapped dispatch table --
# the last removed 35M host callbacks and 12.6% of all PIC misses for exactly zero time). The
# common failure was pricing work by COUNTING EVENTS. This measures TIME and CPU only.
#
# The number that decides the goal is not a hotspot, it is the SERIAL FRACTION. wall < 20 with
# blocks A+B and C running on one core means those blocks must fit in the budget by themselves,
# because no amount of ring parallelism touches them. cpu/wall gives the average core count over
# the whole boot; the per-phase spans say which part is stuck at 1.0.
#
# Reference, same machine and affinity: JVM wall=6.7s cpu=20.8s Done=0.187s.
param([int]$TimeoutS=400,[int]$Runs=3)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
# EVERY diagnostic disarmed. Each of these is a contended per-dispatch atomic and one of them
# armed has three separate times produced a bogus number here.
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE',
              'RAVA_PROFILE_PC_STACKS','RAVA_EXEC_THREADS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null

function Stamp($log,$pat){
  $x=Select-String -Path $log -Pattern $pat -EA SilentlyContinue | Select-Object -First 1
  if($x -and $x.Line -match '^\[(\d\d:\d\d:\d\d)\]'){ $Matches[1] }
}
function Span($a,$b){
  if(-not $a -or -not $b){return $null}
  $d=([datetime]$b-[datetime]$a).TotalSeconds; if($d -lt 0){$d+=86400}; [math]::Round($d,1)
}

$rows=@()
for($i=1; $i -le $Runs; $i++){
  robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
  $log="$here\wallbudget_$i.log"
  $sw=[Diagnostics.Stopwatch]::StartNew()
  $p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]0x0FFF
  while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
    if($p.HasExited){break}
    Start-Sleep -Milliseconds 250
    if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
  }
  $wall=[math]::Round($sw.Elapsed.TotalSeconds,1)
  try { $p.Refresh(); $cpu=[math]::Round($p.TotalProcessorTime.TotalSeconds,1) } catch { $cpu=0 }
  Start-Sleep -Milliseconds 400
  if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  $m = Select-String $log -Pattern 'Done \(([0-9.]+)s\)' | Select-Object -First 1
  $done = if($m){ [double]$m.Matches.Groups[1].Value } else { -1 }
  $t0=Stamp $log '^\['; $tE=Stamp $log 'Environment:'; $tS=Stamp $log 'Starting minecraft server version'
  $ab=Span $t0 $tE; $c=Span $tE $tS
  $cores = if($wall -gt 0){ [math]::Round($cpu/$wall,2) } else { 0 }
  Write-Host ("run{0}  wall={1}s  cpu={2}s  cores={3}  A+B={4}s  C={5}s  Done={6}s" -f $i,$wall,$cpu,$cores,$ab,$c,$done)
  $rows += [pscustomobject]@{run=$i;wall=$wall;cpu=$cpu;cores=$cores;ab=$ab;c=$c;done=$done}
}
powercfg /setactive $orig | Out-Null
$rows | Export-Csv "$here\wall_budget.csv" -NoTypeInformation
$best = $rows | Sort-Object wall | Select-Object -First 1
Write-Host ""
Write-Host ("BEST  wall={0}s  cpu={1}s  avg_cores={2}  A+B={3}s  C={4}s  Done={5}s" -f $best.wall,$best.cpu,$best.cores,$best.ab,$best.c,$best.done)
$serial = 0.0 + $best.ab + $best.c
Write-Host ("SERIAL FRACTION (A+B + C, both single-threaded) = {0}s of {1}s wall = {2}%" -f `
  $serial,$best.wall,[math]::Round(100*$serial/$best.wall,1))
Write-Host ("=> even with Done at ZERO, wall could not go below {0}s. Target is 20s." -f $serial)
Write-Host "JVM reference on the same box/affinity: wall=6.7s cpu=20.8s Done=0.187s"

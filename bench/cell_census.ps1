# HOW FAR IS THE LIVE GRAPH FROM THE BARE-DEREF FLIP, ON THE CURRENT TREE?
#
# The value-model arc pays only at its final flip, which is legal only when no non-0b00 ref cell
# can persist. So the meaningful progress metric for the arc is not wall time (steps 1-5 have no
# wall win by construction) but the BLOCKING CELL COUNT, and RAVA_CELL_CENSUS reports it directly:
# 0b00 (bare-ready) vs handle / array / box, plus the CONTENT of the array and box buckets and the
# classes that own the residue.
#
# Two questions here. Did value-model step 1 (box-arm retirement, RAVA_ARENA_STR_REFCELL, shipped
# default-ON) actually empty the box bucket on a real boot? And what is the residue NOW, so step 2
# is aimed by measurement rather than by a census taken on 2026-07-24 against a very different tree.
#
# The census walks the live graph during collections and prints on a new peak, so the LAST line is
# the high-water reading. Its own cost makes the wall meaningless - counts only.
param([int]$TimeoutS=600)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
$exe="$root\target\release\rava.exe"
Assert-FreshExe $exe
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE',
              'RAVA_PROFILE_PC','RAVA_PROFILE_WALL','RAVA_JIT_STATS','RAVA_EXEC_THREADS',
              'RAVA_INLINE_DEPTH'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_CELL_CENSUS='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\cellcensus.log"
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 400
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
Start-Sleep -Milliseconds 500
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
Write-Host ("(census armed, {0}s wall -- COUNTS only)" -f [math]::Round($sw.Elapsed.TotalSeconds,1))
Write-Host ""
Write-Host "=== high-water cell census (last reading) ==="
$l = Get-Content "$log.err" -EA SilentlyContinue
$idx = ($l | Select-String 'CELL-CENSUS. ref cells' | Select-Object -Last 1)
if($idx){
  $start = $idx.LineNumber - 1
  for($j=$start; $j -lt [Math]::Min($start+26,$l.Count); $j++){
    if($j -gt $start -and $l[$j] -match 'CELL-CENSUS. ref cells'){ break }
    Write-Host $l[$j]
  }
} else { Write-Host "no [CELL-CENSUS] lines - check the gate"; $l | Select-String 'CENSUS' | Select-Object -First 5 }

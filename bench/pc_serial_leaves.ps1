# WHAT IS THE ~10.5s OF NATIVE_BRIDGES IN THE SERIAL WINDOW?
#
# The serial 54s (68% of wall, bench/wall_budget.ps1) splits JIT_EXEC 62% / NATIVE_BRIDGES 21% /
# INTERP 11% / VM_DISPATCH 5.5% / CLASS_LOADING 0.7%. Four of those five are the guest executing
# through our machinery; NATIVE_BRIDGES is ~10.5s of OUR OWN RUST -- gson / R2OMAP / IO -- and it
# has never been broken down. This tree has form there: the key-intern find was -81% on
# RandomState.create because a "pure" memo cached nothing and key derivation ran over 21 MB of
# flattened text per call. A quadratic in a native is the one kind of big cheap win still open.
#
# RAVA_PROFILE_PC dumps host-side leaves by FUNCTION per block. Blocks are 12s so the early ones
# are the serial window (A+B ends at "Environment:", C at "Starting minecraft server version");
# the last block is the ring, where the main thread is parked in supplyAsync and the host rows
# are wait, not work.
#
# release-diag: `release` mis-names symbols (strip="symbols"), which has produced wrong leaf
# names here before.
param([int]$TimeoutS=400,[int]$EveryMs=12000)
$root='D:\code\rava'; $here="$root\bench"
$exe="$root\target\release-diag\rava.exe"
$cp='out;'+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_FORK_STATS','RAVA_LANE_PROBE','RAVA_LANE_OUTER','RAVA_CALLBACK_STATS',
              'RAVA_JIT_PIC_PROBE','RAVA_IC_VTABLE_FALLBACK','RAVA_DISPATCH_TABLE',
              'RAVA_PROFILE_PC_STACKS','RAVA_EXEC_THREADS'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';
        RAVA_PROFILE_PC='1';RAVA_PROFILE_WALL_EVERY_MS="$EveryMs"}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$here\pcserial.log"
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 300
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
Start-Sleep -Milliseconds 400
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
Write-Host ("wall={0}s (sampler armed -- the LEAVES are the output)" -f [math]::Round($sw.Elapsed.TotalSeconds,1))
Write-Host ""
Write-Host "=== host-side leaves, EARLY blocks (the serial window) ==="
$lines = Get-Content "$log.err" -EA SilentlyContinue
$idx = 0
for($i=0; $i -lt $lines.Count; $i++){
  if($lines[$i] -match 'host side \((\d+) samples'){
    $idx++
    if($idx -le 4){
      Write-Host ("--- host-side block #{0} ---" -f $idx)
      for($j=$i; $j -lt [Math]::Min($i+13,$lines.Count); $j++){ Write-Host $lines[$j] }
      Write-Host ""
    }
  }
}

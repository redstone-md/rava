# DOES CALLBUF SHARING ACTUALLY REMOVE SLOTS? Same counting on both arms.
#
# Last turn's "17 -> 16" was an artifact: 17 counted ALL explicit slots (including ss0, the
# 56-byte operand buffer) while 16 counted only the 512-byte ones. Comparing the two is
# meaningless. This runs the SAME dump and the SAME count with RAVA_CALLBUF_SHARE off and on.
#
# 512 B == dispatch_callbuf_slots() * 8, and a grep of jit.rs says every slot of that size is now
# created through `shared_slot`. So with sharing ON the count must collapse to at most 2 (one
# slot-laid, one packed). If it does not, the slots come from somewhere the grep did not find and
# the whole premise of the change is wrong.
$root='D:\code\rava'
function Count-Slots {
  param([string]$Share)
  if($Share){ $env:RAVA_CALLBUF_SHARE='1' } else { Remove-Item env:RAVA_CALLBUF_SHARE -EA SilentlyContinue }
  Remove-Item "$root\clif.txt" -EA SilentlyContinue
  & "$root\bench\ring_hot_method.ps1" -N 4 -Dump 'DensityFunctions' *> $null
  $l=Get-Content "$root\clif.txt"
  $hdrs=@(); for($i=0;$i -lt $l.Count;$i++){ if($l[$i] -match '^==== CLIF (.+) ====$'){ $hdrs += [pscustomobject]@{i=$i;h=$Matches[1]} } }
  $t = $hdrs | Where-Object { $_.h -match 'ShiftedNoise\.\(Lnet/minecraft/world/level/levelgen/DensityFunction\$FunctionContext;\)D' } | Select-Object -First 1
  if(-not $t){ return $null }
  $end = ($hdrs | Where-Object { $_.i -gt $t.i } | Select-Object -First 1); $stop = if($end){ $end.i } else { $l.Count }
  $body = $l[($t.i)..($stop-1)]
  [pscustomobject]@{
    s512  = @($body | Where-Object { $_ -match 'explicit_slot 512' }).Count
    sAll  = @($body | Where-Object { $_ -match 'explicit_slot' }).Count
    lines = $body.Count
  }
}
$off = Count-Slots $false
$on  = Count-Slots $true
Remove-Item env:RAVA_CALLBUF_SHARE -EA SilentlyContinue
Write-Host ""
Write-Host "DensityFunctions`$ShiftedNoise.compute(FunctionContext)D"
Write-Host ("  SHARE off : 512B slots={0}  all slots={1}  lines={2}" -f $off.s512,$off.sAll,$off.lines)
Write-Host ("  SHARE on  : 512B slots={0}  all slots={1}  lines={2}" -f $on.s512,$on.sAll,$on.lines)
Write-Host ""
if($on.s512 -lt $off.s512){ Write-Host ("=> sharing removed {0} slots ({1} B of frame)" -f ($off.s512-$on.s512),(512*($off.s512-$on.s512))) }
else { Write-Host "=> sharing removed NOTHING - the 512 B slots are not created by the rewired sites" -ForegroundColor Yellow }

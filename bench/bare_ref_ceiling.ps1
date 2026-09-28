# WHAT DOES THE VALUE-MODEL ARC'S FINAL FLIP ACTUALLY BUY, IN INSTRUCTIONS?
#
# The whole strategic case for that arc rests on one claim: the ~1454
# band/icmp/select/cmov/sete/testb tag-discrimination instructions in the ring's hottest compiled
# method (DensityFunctions$ShiftedNoise.compute, 852 blocks, ~8900 instructions for four lines of
# Java) disappear at the FINAL bare-dereference flip, and that is what closes a 4x gap no other
# lever can. That claim has never been priced - it has only ever been asserted, including by me.
#
# RAVA_JIT_BARE_REF_LOAD is the emitter side of that flip and already exists (three sites in
# jit.rs). It is UNSOUND before 100% handle retirement - a non-0b00 cell would be dereferenced raw
# - so this must never be judged by running the workload. But the CEILING is visible without
# running anything: compile the same method both ways and count what the emitter drops.
#
# That makes this measurable on a machine that cannot currently time anything, which is the point:
# instruction counts are load-independent, and this prices the arc's payoff rather than assuming it.
#
# NOT A CORRECTNESS RUN. The gate hash is expected to diverge or the run to die; only clif.txt
# matters, and it is written at COMPILE time, before any bad dereference can happen.
# TARGET CLASS. DensityFunctions cannot be used: with the flag ON the process dies before that
# class ever compiles (an unsound bare dereference of a non-0b00 cell, which is exactly what the
# flag is documented to be before 100%% retirement), so clif.txt comes back empty. Any class
# compiled EARLY enough to survive both arms gives the same per-instruction answer, since the
# emitter change is per ref-field-load and not specific to the density tree.
param([int]$N=4,[string]$Class='RingScan',[string]$Method='')
$root='D:\code\rava'; $here="$root\bench"
function Dump-Method {
  param([string]$Bare)
  if($Bare){ $env:RAVA_JIT_BARE_REF_LOAD='1' } else { Remove-Item env:RAVA_JIT_BARE_REF_LOAD -EA SilentlyContinue }
  Remove-Item "$root\clif.txt" -EA SilentlyContinue
  & "$here\ring_hot_method.ps1" -N $N -Dump $Class *> $null
  if(-not (Test-Path "$root\clif.txt")){ return $null }
  $l=Get-Content "$root\clif.txt"
  $hdrs=@(); for($i=0;$i -lt $l.Count;$i++){ if($l[$i] -match '^==== CLIF (.+) ====$'){ $hdrs += [pscustomobject]@{i=$i;h=$Matches[1]} } }
  # Largest compiled body in the dump (most ref loads => the clearest signal), unless -Method pins one.
  $cands = if($Method){ $hdrs | Where-Object { $_.h -match [regex]::Escape($Method) } } else { $hdrs }
  $t = $null; $best = -1
  foreach($h in $cands){
    $e = ($hdrs | Where-Object { $_.i -gt $h.i } | Select-Object -First 1)
    # PowerShell 5.1 has no if-as-expression; assign then use.
    $stopI = $l.Count
    if($e){ $stopI = $e.i }
    $sz = $stopI - $h.i
    if($sz -gt $best){ $best = $sz; $t = $h }
  }
  if(-not $t){ return $null }
  $end = ($hdrs | Where-Object { $_.i -gt $t.i } | Select-Object -First 1); $stop = if($end){ $end.i } else { $l.Count }
  $body = $l[($t.i)..($stop-1)]
  $ops=@{}; $blocks=0
  foreach($ln in $body){
    if($ln -match '^block\d+'){ $blocks++ }
    if($ln -match '^\s+(?:v\d+(?:, v\d+)*\s*=\s*)?([a-z_][a-z0-9_.]*)\b'){ $op=$Matches[1]; if($op -ne 'function'){ $ops[$op]=[int]$ops[$op]+1 } }
  }
  $tag=0; foreach($k in @('band','icmp','icmp_imm','select','cmovneq','sete','setg','testb','testq')){ $tag += [int]$ops[$k] }
  [pscustomobject]@{ lines=$body.Count; blocks=$blocks; tag=$tag; movq=[int]$ops['movq']; total=(($ops.Values)|Measure-Object -Sum).Sum }
}
$off = Dump-Method $false
$on  = Dump-Method $true
Remove-Item env:RAVA_JIT_BARE_REF_LOAD -EA SilentlyContinue
if(-not $off -or -not $on){ Write-Host "could not dump both arms (method not compiled in one of them)"; exit 1 }
Write-Host ""
Write-Host ("target class: {0}   (largest compiled body in the dump)" -f $Class)
Write-Host ("  BARE off : lines={0,-7} blocks={1,-5} tag_ops={2,-6} movq={3,-6} total_ops={4}" -f $off.lines,$off.blocks,$off.tag,$off.movq,$off.total)
Write-Host ("  BARE on  : lines={0,-7} blocks={1,-5} tag_ops={2,-6} movq={3,-6} total_ops={4}" -f $on.lines,$on.blocks,$on.tag,$on.movq,$on.total)
Write-Host ""
$dt = $off.tag - $on.tag; $dtot = $off.total - $on.total
$pct = if($off.tag -gt 0){ [math]::Round(100.0*$dt/$off.tag,1) } else { 0 }
$ptot = if($off.total -gt 0){ [math]::Round(100.0*$dtot/$off.total,1) } else { 0 }
Write-Host ("=> tag ops removed: {0} of {1} ({2}%)" -f $dt,$off.tag,$pct)
Write-Host ("=> TOTAL ops removed: {0} of {1} ({2}%)  <- the arc's instruction-level ceiling" -f $dtot,$off.total,$ptot)
Write-Host "   (a ceiling, not a speedup: removed instructions are not uniformly priced)"

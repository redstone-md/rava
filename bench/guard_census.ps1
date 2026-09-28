# WHICH GUARD EMITS THE 534 BRANCH-ONLY BLOCKS?
#
# bench/block_anatomy.ps1 established that the ring's hottest compiled method is a mesh of 852
# four-instruction blocks, 62.7% of which do no memory work and no call - they exist only to route
# control flow - and that this, not tag discrimination, is what the 6.6x CPU gap against HotSpot is
# made of (the value-model flip removes only ~5.6% of instructions, ~5% of blocks).
#
# "Control-flow density" is a diagnosis, not a lever. To become a lever it has to name WHICH guard
# emits those blocks: a null check, an arena-vs-handle classify, a COW/ring store barrier, a GC
# forwarding check, a bounds check, a PIC class compare and a deep/leaf dispatch arm are all
# separate mechanisms with separate fixes, and they are indistinguishable in a block count.
#
# So classify each branch-only block by the SHAPE of the value its terminator branches on, and print
# verbatim samples so the classification can be checked by eye rather than trusted.
#
# Load-independent: this reads emitted code, not timings.
# NOTE: $sampleMap, not $samples -- PowerShell variable names are CASE-INSENSITIVE, so a
# `$samples` hashtable silently collides with the `[int]$Samples` parameter and PowerShell tries to
# coerce the hashtable to an int. The error surfaces far from the cause.
param([string]$Class='DensityFunctions',[string]$MethodLike='ShiftedNoise',[int]$Samples=6)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
Assert-FreshExe "$root\target\release-diag\rava.exe"
$cp="$here\out;"+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_JIT_BARE_REF_LOAD','RAVA_PROFILE_PC','RAVA_PROFILE_WALL'){ Remove-Item "env:$k" -EA SilentlyContinue }
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_JIT_TIER0='1'
$env:RAVA_JIT_DUMP=$Class
Remove-Item "$root\clif.txt" -EA SilentlyContinue
Push-Location $root
& "$root\target\release-diag\rava.exe" -cp $cp RingScan 4 *> $null
Pop-Location
Remove-Item env:RAVA_JIT_DUMP -EA SilentlyContinue
if(-not (Test-Path "$root\clif.txt")){ Write-Host "no dump"; exit 1 }

$l = Get-Content "$root\clif.txt"
$h=@(); for($i=0;$i -lt $l.Count;$i++){ if($l[$i] -match '^==== CLIF (.+) ====$'){ $h += [pscustomobject]@{i=$i;n=$Matches[1]} } }
$cands = $h | Where-Object { $_.n -match $MethodLike }
if(-not $cands){ Write-Host "method not in dump"; exit 1 }
$t=$null; $best=-1
foreach($x in $cands){
  $e = ($h | Where-Object { $_.i -gt $x.i } | Select-Object -First 1)
  $s = $l.Count; if($e){ $s = $e.i }
  if(($s-$x.i) -gt $best){ $best = $s-$x.i; $t=$x }
}
$e = ($h | Where-Object { $_.i -gt $t.i } | Select-Object -First 1); $stop=$l.Count; if($e){ $stop=$e.i }
$body = $l[($t.i)..($stop-1)]

$starts=@(); for($i=0;$i -lt $body.Count;$i++){ if($body[$i] -match '^block\d+'){ $starts += $i } }
# Only the CLIF section: past the machine-code half the same text repeats in a different syntax.
$kinds=@{}; $sampleMap=@{}
for($k=0;$k -lt $starts.Count;$k++){
  $s=$starts[$k]; $en=$body.Count; if($k+1 -lt $starts.Count){ $en=$starts[$k+1] }
  $blk = @($body[$s..($en-1)])
  $ins = @($blk | Where-Object { $_ -match '^\s+\S' })
  if(@($ins | Where-Object { $_ -match '\bcall\b|call_indirect' }).Count -gt 0){ continue }
  if(@($ins | Where-Object { $_ -match '\bload\b|\bstore\b' }).Count -gt 0){ continue }
  $txt = ($ins -join ' ; ')
  # Classify by the guard SHAPE, most specific first.
  $kind =
    if($txt -match 'band\s+\w+,\s*7|band_imm\s+\w+,\s*7')          { 'tag classify (band 7)' }
    elseif($txt -match 'ushr_imm\s+\w+,\s*40|icmp_imm.*1099511627776') { 'REF_TAG / high-bits (>>40)' }
    elseif($txt -match 'icmp_imm\s+eq\s+\w+,\s*0|icmp_imm.*,\s*0\b') { 'null / zero test' }
    elseif($txt -match 'icmp_imm\s+(s|u)?g[te]')                    { 'bounds / positive test' }
    elseif($txt -match 'icmp\b')                                    { 'value compare (PIC class id?)' }
    elseif($txt -match 'brif')                                      { 'other conditional' }
    else                                                            { 'unconditional / merge' }
  $kinds[$kind] = [int]$kinds[$kind] + 1
  if(-not $sampleMap.ContainsKey($kind)){ $sampleMap[$kind] = @() }
  if($sampleMap[$kind].Count -lt $Samples){ $sampleMap[$kind] += ,$blk }
}
$tot = ($kinds.Values | Measure-Object -Sum).Sum
Write-Host ""
Write-Host ("method: {0}" -f $t.n)
Write-Host ("branch-only blocks classified: {0}" -f $tot)
Write-Host ""
foreach($kv in ($kinds.GetEnumerator() | Sort-Object Value -Descending)){
  Write-Host ("  {0,-32} {1,5}  ({2}%)" -f $kv.Key,$kv.Value,[math]::Round(100.0*$kv.Value/[math]::Max($tot,1),1))
}
Write-Host ""
Write-Host "=== verbatim samples (check the classification by eye) ==="
foreach($kv in ($kinds.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 3)){
  Write-Host ("--- {0} ---" -f $kv.Key)
  $sn = $sampleMap[$kv.Key]
  for($j=0; $j -lt [Math]::Min(2,$sn.Count); $j++){
    foreach($line in $sn[$j]){ Write-Host ("    " + $line.TrimEnd()) }
    Write-Host ""
  }
}

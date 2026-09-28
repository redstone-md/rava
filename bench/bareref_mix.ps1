# THE VALUE-MODEL ARC'S CEILING, MEASURED - on the one workload where the flip is SOUND.
#
# RAVA_JIT_BARE_REF_LOAD emits a bare `load` for an arena ref field instead of the tag-classify
# diamond: it IS the value-model rewrite's final flip, on the emitter side. Two earlier attempts to
# price it failed for opposite reasons - DensityFunctions dies before compiling (the flag is unsound
# while any 0b01/0b11 cell exists), and RingScan compiles byte-identically (its hot body is numeric
# and never touches the arena ref-field emitters).
#
# bench/BareRef.java is built to satisfy both constraints: every object is its own plain class,
# allocated locally with a complete layout and no native-backed super, so all refs are arena-born
# 0b00 and the flag is SOUND here (verified: both arms print BAREREF_HASH=71199712585cb2a0); and the
# hot loop is nothing but reference-field reads and writes, which is exactly what the flag gates.
#
# Instruction counts are LOAD-INDEPENDENT, so this works on a machine too busy to time anything.
# What it yields is a CEILING and should never be quoted as the arc's value on the real boot: here
# every ref is already 0b00, which is the end state the arc spends years reaching.
param([int]$N=512,[int]$Steps=400000,[string]$Method='(LBareRef$Node;I)J')
$root='D:\code\rava'
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'; $env:RAVA_JIT_TIER0='1'

function Get-Mix {
  param([bool]$Bare)
  if($Bare){ $env:RAVA_JIT_BARE_REF_LOAD='1' } else { Remove-Item env:RAVA_JIT_BARE_REF_LOAD -EA SilentlyContinue }
  Remove-Item "$root\clif.txt" -EA SilentlyContinue
  $env:RAVA_JIT_DUMP='BareRef'
  # clif.txt is written relative to the RAVA PROCESS's working directory, not the script's.
  # Running from bench/ silently put the dump in bench/clif.txt while the reader looked in the
  # repo root, so it reported "dump missing" for a dump that existed. Pin the cwd.
  Push-Location $root
  & "$root\target\release-diag\rava.exe" -cp "$root\bench\out" BareRef $N $Steps *> $null
  Pop-Location
  Remove-Item env:RAVA_JIT_DUMP -EA SilentlyContinue
  if(-not (Test-Path "$root\clif.txt")){ return $null }
  $l = Get-Content "$root\clif.txt"
  $h=@(); for($i=0;$i -lt $l.Count;$i++){ if($l[$i] -match '^==== CLIF (.+) ====$'){ $h += [pscustomobject]@{i=$i;n=$Matches[1]} } }
  # Pin the HOT LOOP by descriptor. Picking the largest body selects main(), whose setup
  # loops dilute the ref-field signal this is trying to price.
  $cands = $h | Where-Object { $_.n -like ('*' + $Method) }
  if(-not $cands){ $cands = $h }
  $t=$null; $best=-1
  foreach($x in $cands){
    $e = ($h | Where-Object { $_.i -gt $x.i } | Select-Object -First 1)
    $s = $l.Count; if($e){ $s = $e.i }
    if(($s - $x.i) -gt $best){ $best = $s - $x.i; $t = $x }
  }
  if(-not $t){ return $null }
  $e = ($h | Where-Object { $_.i -gt $t.i } | Select-Object -First 1); $stop=$l.Count; if($e){ $stop=$e.i }
  $body = $l[($t.i)..($stop-1)]
  $ops=@{}; $blocks=0
  $opRe = [regex]'^\s+(?:v\d+(?:, v\d+)*\s*=\s*)?([a-z_][a-z0-9_.]*)'
  foreach($ln in $body){
    if($ln -match '^block\d+'){ $blocks++ }
    $m = $opRe.Match($ln)
    if($m.Success){ $o = $m.Groups[1].Value; if($o -ne 'function'){ $ops[$o] = [int]$ops[$o] + 1 } }
  }
  $tag=0; foreach($k in @('band','icmp','icmp_imm','select','cmovneq','sete','setg','testb','testq')){ $tag += [int]$ops[$k] }
  [pscustomobject]@{ name=$t.n; lines=$body.Count; blocks=$blocks; tag=$tag; total=(($ops.Values)|Measure-Object -Sum).Sum }
}

$off = Get-Mix $false
$on  = Get-Mix $true
Remove-Item env:RAVA_JIT_BARE_REF_LOAD -EA SilentlyContinue
if(-not $off -or -not $on){ Write-Host "dump missing in one arm"; exit 1 }
Write-Host ""
Write-Host ("method: {0}" -f $off.name)
Write-Host ("  BARE off : lines={0,-6} blocks={1,-4} tag_ops={2,-5} total_ops={3}" -f $off.lines,$off.blocks,$off.tag,$off.total)
Write-Host ("  BARE on  : lines={0,-6} blocks={1,-4} tag_ops={2,-5} total_ops={3}" -f $on.lines,$on.blocks,$on.tag,$on.total)
Write-Host ""
$dt = $off.tag - $on.tag; $dd = $off.total - $on.total
Write-Host ("=> tag ops removed  : {0} of {1} ({2}%)" -f $dt,$off.tag,[math]::Round(100.0*$dt/[math]::Max($off.tag,1),1))
Write-Host ("=> TOTAL ops removed: {0} of {1} ({2}%)   <- instruction-level CEILING" -f $dd,$off.total,[math]::Round(100.0*$dd/[math]::Max($off.total,1),1))
Write-Host ("=> blocks removed   : {0} of {1}" -f ($off.blocks-$on.blocks),$off.blocks)
Write-Host "   (a ceiling on a best case: every ref here is already 0b00, which is where the arc ENDS)"

# WHAT ARE THE 852 BLOCKS, IF NOT TAG CHECKS?
#
# The value-model arc's final flip is now priced at ~5.6% of instructions on the most favourable
# possible workload (bench/bareref_mix.ps1), which does NOT support "only that arc closes the 6.6x
# CPU gap against HotSpot". So the gap comes from somewhere else, and the ring's hottest compiled
# method is where to look: DensityFunctions$ShiftedNoise.compute(FunctionContext)D is 852 BLOCKS
# and ~8900 instructions for four lines of Java, with 2555 movq.
#
# 852 blocks is the number never explained. A straight-line numeric method should have a handful.
# This classifies every block by what it CONTAINS, which distinguishes the two candidate stories:
#
#   * if most blocks contain a CALL, the body is a chain of guarded fast paths each with a host
#     callback arm -- the cost is the guard/fallback ARCHITECTURE, not the value model, and the
#     lever is making operations straight-line;
#   * if most are small branch-only blocks, it is control-flow shape from the tag/null/bounds
#     diamonds, which the value model does address.
#
# Load-independent: block classification is a property of the emitted code, not of the machine.
param([string]$Class='DensityFunctions',[string]$MethodLike='ShiftedNoise')
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
Assert-FreshExe "$root\target\release-diag\rava.exe"
$cp="$here\out;"+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_JIT_BARE_REF_LOAD','RAVA_PROFILE_PC','RAVA_PROFILE_WALL'){ Remove-Item "env:$k" -EA SilentlyContinue }
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_JIT_TIER0='1'
$env:RAVA_JIT_DUMP=$Class
Remove-Item "$root\clif.txt" -EA SilentlyContinue
Push-Location $root   # clif.txt lands in the RAVA PROCESS's cwd, not the script's
& "$root\target\release-diag\rava.exe" -cp $cp RingScan 4 *> $null
Pop-Location
Remove-Item env:RAVA_JIT_DUMP -EA SilentlyContinue
if(-not (Test-Path "$root\clif.txt")){ Write-Host "no dump produced"; exit 1 }

$l = Get-Content "$root\clif.txt"
$h=@(); for($i=0;$i -lt $l.Count;$i++){ if($l[$i] -match '^==== CLIF (.+) ====$'){ $h += [pscustomobject]@{i=$i;n=$Matches[1]} } }
$cands = $h | Where-Object { $_.n -match $MethodLike }
if(-not $cands){ Write-Host "method not in dump"; exit 1 }
$t=$null; $best=-1
foreach($x in $cands){
  $e = ($h | Where-Object { $_.i -gt $x.i } | Select-Object -First 1)
  $s = $l.Count; if($e){ $s = $e.i }
  if(($s - $x.i) -gt $best){ $best = $s - $x.i; $t = $x }
}
$e = ($h | Where-Object { $_.i -gt $t.i } | Select-Object -First 1); $stop=$l.Count; if($e){ $stop=$e.i }
$body = $l[($t.i)..($stop-1)]

# Walk CLIF blocks by INDEX RANGE. The obvious accumulator form is a PowerShell trap:
# `$cur -ne $null` on an ARRAY compares element-wise and yields an empty array (falsy), so
# nothing is ever appended and every block reads as empty. Index ranges avoid it entirely.
$starts = @()
for($i=0; $i -lt $body.Count; $i++){ if($body[$i] -match '^block\d+'){ $starts += $i } }
$blocks = @()
for($k=0; $k -lt $starts.Count; $k++){
  $s = $starts[$k]
  $e = $body.Count; if($k+1 -lt $starts.Count){ $e = $starts[$k+1] }
  $blocks += ,@($body[$s..($e-1)])
}
$withCall=0; $withLoad=0; $branchOnly=0; $tiny=0; $sizes=@()
foreach($b in $blocks){
  $ins = @($b | Where-Object { $_ -match '^\s+\S' })
  $sizes += $ins.Count
  $hasCall = @($ins | Where-Object { $_ -match '\bcall\b|call_indirect' }).Count -gt 0
  $hasLoad = @($ins | Where-Object { $_ -match '\bload\b|\bstore\b' }).Count -gt 0
  if($hasCall){ $withCall++ }
  # Memory-but-no-call, counted DISJOINTLY. Subtracting withCall from withLoad was wrong -- they
  # are overlapping sets, not nested, and it printed a negative percentage.
  if($hasLoad -and -not $hasCall){ $withLoad++ }
  if(-not $hasCall -and -not $hasLoad){ $branchOnly++ }
  if($ins.Count -le 3){ $tiny++ }
}
$n=$blocks.Count
Write-Host ""
Write-Host ("method: {0}" -f $t.n)
$med = 0
if($n -gt 0){ $sorted = @($sizes | Sort-Object); $med = $sorted[[int]($n/2)] }
Write-Host ("blocks={0}  instrs={1}  median block={2} instrs" -f $n,($sizes|Measure-Object -Sum).Sum,$med)
Write-Host ""
Write-Host ("  contain a CALL (host callback / dispatch) : {0,5}  ({1}%)" -f $withCall,[math]::Round(100.0*$withCall/[math]::Max($n,1),1))
Write-Host ("  contain a load/store, no call             : {0,5}  ({1}%)" -f $withLoad,[math]::Round(100.0*$withLoad/[math]::Max($n,1),1))
Write-Host ("  branch-only (no memory, no call)          : {0,5}  ({1}%)" -f $branchOnly,[math]::Round(100.0*$branchOnly/[math]::Max($n,1),1))
Write-Host ("  tiny (<=3 instrs)                         : {0,5}  ({1}%)" -f $tiny,[math]::Round(100.0*$tiny/[math]::Max($n,1),1))
Write-Host ""
Write-Host "read: many CALL blocks => guarded fast path + host fallback ARCHITECTURE (not the value model)"
Write-Host "      many branch-only  => diamond control-flow shape (which the value model does address)"

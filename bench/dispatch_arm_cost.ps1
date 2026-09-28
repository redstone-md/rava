# WHICH OF THE 12 BLOCKS PER DISPATCH SITE BELONG TO WHICH ARM?
#
# Measured: emit_dispatch_pic creates exactly 12.0 blocks per call in a default build, and the ring's
# hottest method (103 call sites, ~65% emitting a PIC) has 852 blocks - i.e. ~67 x 12 = ~804 of them
# ARE dispatch sites. That makes "12" the number to attack, but 12 is a total, and the arms have
# different justifications and different kill switches.
#
# The heap-Ref arm (refpic / refhit / refdeep / refcheck / ref_split) exists only at heap_mode 2,
# which is `RAVA_NO_JIT_HEAPPIC` unset - default-ON. Toggling that kill switch and re-reading the
# per-call block count prices the arm exactly, with no guessing about which create_block() sites
# actually fire.
#
# Load-independent: block counts are a property of emitted code, so this works on a busy machine.
# NOT a correctness or speed claim - RAVA_NO_JIT_HEAPPIC changes dispatch behaviour and is recorded
# as boot -2.6% / FILL +12.7%. This measures CODE SHAPE only.
param([int]$N=4)
$root='D:\code\rava'; $here="$root\bench"
. "$here\assert_fresh_exe.ps1"
Assert-FreshExe "$root\target\release-diag\rava.exe"
$cp="$here\out;"+((Get-Content "$here\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_JIT_BARE_REF_LOAD','RAVA_PROFILE_PC','RAVA_PROFILE_WALL','RAVA_DISPATCH_TABLE',
              'RAVA_M3_IC_PARALLEL','RAVA_JIT_FLAT_TAGSELECT'){ Remove-Item "env:$k" -EA SilentlyContinue }
$base=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';
        RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';RAVA_JIT_BLOCK_CENSUS='1'}
foreach($k in $base.Keys){ Set-Item "env:$k" $base[$k] }

function Measure-Blocks {
  param([bool]$NoHeapPic)
  if($NoHeapPic){ $env:RAVA_NO_JIT_HEAPPIC='1' } else { Remove-Item env:RAVA_NO_JIT_HEAPPIC -EA SilentlyContinue }
  Remove-Item "$root\clif.txt" -EA SilentlyContinue
  $env:RAVA_JIT_DUMP='ShiftedNoise'
  Push-Location $root   # clif.txt lands in the rava process's cwd
  & "$root\target\release-diag\rava.exe" -cp $cp RingScan $N *> $null
  Pop-Location
  Remove-Item env:RAVA_JIT_DUMP -EA SilentlyContinue
  if(-not (Test-Path "$root\clif.txt")){ return $null }
  $line = Get-Content "$root\clif.txt" | Select-String 'BLOCK-CENSUS' | Select-Object -Last 1
  if(-not $line){ return $null }
  if($line.Line -match '(\d+) calls, (\d+) blocks REALLY created \(([0-9.]+)/call\)'){
    [pscustomobject]@{ calls=[int]$Matches[1]; blocks=[int]$Matches[2]; per=[double]$Matches[3] }
  } else { $null }
}

$on  = Measure-Blocks $false   # heap-Ref PIC ON (default)
$off = Measure-Blocks $true    # heap-Ref PIC OFF
Remove-Item env:RAVA_NO_JIT_HEAPPIC -EA SilentlyContinue
if(-not $on -or -not $off){ Write-Host "census line missing in one arm"; exit 1 }
Write-Host ""
Write-Host ("heap-Ref PIC ON  (default): {0} calls, {1} blocks, {2}/call" -f $on.calls,$on.blocks,$on.per)
Write-Host ("heap-Ref PIC OFF          : {0} calls, {1} blocks, {2}/call" -f $off.calls,$off.blocks,$off.per)
Write-Host ""
$d = $on.per - $off.per
Write-Host ("=> the heap-Ref arm costs {0} blocks per dispatch site, of {1} ({2}%)" -f `
  [math]::Round($d,1),$on.per,[math]::Round(100.0*$d/[math]::Max($on.per,0.001),0))
Write-Host "   (code shape only - this kill switch also changes dispatch behaviour: boot -2.6%, FILL +12.7%)"

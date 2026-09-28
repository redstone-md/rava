# max_locals + max_stack for the callees that anchor the ring, because that sum is
# EXACTLY the predicate `dispatch_resolve_cacheable_cls` uses: a non-leaf callee whose
# sum exceeds `dispatch_callbuf_slots()` is NeverCacheable PERMANENTLY, for every site
# and every receiver, so every call to it leaves compiled code forever.
# Default width is 64 (jit.rs), and the CALLBUF-WIDE note already says the lane's
# anchor method does not fit even there.
param([string[]]$Classes = @(
  'net.minecraft.world.level.levelgen.synth.ImprovedNoise',
  'net.minecraft.world.level.levelgen.synth.PerlinNoise',
  'net.minecraft.world.level.biome.BiomeSource',
  'net.minecraft.world.level.biome.MultiNoiseBiomeSource',
  'net.minecraft.world.level.levelgen.synth.BlendedNoise'
))
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()

foreach ($c in $Classes) {
  # -v is required: the stack=/locals= line lives in the verbose Code attribute.
  $txt = javap -p -v -cp $mccp $c 2>$null
  if (-not $txt) { Write-Host "$c : javap failed"; continue }
  $sig = $null
  foreach ($line in $txt) {
    if ($line -match '^\s{2}\S.*\(.*\);\s*$') { $sig = $line.Trim() }
    elseif ($line -match 'stack=(\d+), locals=(\d+), args_size=(\d+)') {
      $s = [int]$Matches[1]; $l = [int]$Matches[2]
      $sum = $s + $l
      if ($sum -gt 32 -and $sig) {
        Write-Host ("{0,4}  stack={1,3} locals={2,3}  {3}.{4}" -f $sum, $s, $l, $c.Split('.')[-1], $sig)
      }
    }
  }
}

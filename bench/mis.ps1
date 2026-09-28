# 2-SECOND MISCOMPILE REPRODUCER (ledger row 192).
#
# RAVA_BACKGROUND_JIT_QUEUE=32 lets enough methods compile that a latent codegen bug fires and
# the server fails to start in ~2 s, against 45-second boots. `-Deny` passes RAVA_JIT_DENY so a
# suspect family can be kept interpreted and the bisection proceeds one substring at a time.
#
# First signature seen:
#   [ULE-JITMETHOD] Shapes.joinUnoptimized(VoxelShape,VoxelShape,BooleanOp)
#   [ULE-VIRT] invokevirtual MapMakerInternalMap.ordinal()I owner=Direction$Axis
# -- the receiver is a Direction$Axis and the callee resolved in guava, i.e. a constant-pool
# index read against the WRONG class's pool.
param([string]$Deny = '', [string]$Depth = '32', [int]$N = 1)
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
$env:RAVA_PROC_CAP_MB = '4096'
$env:RAVA_HARD_MB = '3584'
$env:RAVA_BACKGROUND_JIT_QUEUE = $Depth
if ($Deny -ne '') { $env:RAVA_JIT_DENY = $Deny }
else { [Environment]::SetEnvironmentVariable('RAVA_JIT_DENY', $null) }
for ($i = 1; $i -le $N; $i++) {
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = 'bench\mis.log'
  Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath 'target\pgo-fat\rava.exe' `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(120)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 150
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $dn = (Get-Content $out | Select-String 'Done \(')
  $fail = (Get-Content $out | Select-String 'Failed to start')
  $verd = if ($dn) { 'BOOTED' } elseif ($fail) { 'CRASH' } else { 'OTHER' }
  $sig = (Get-Content "$out.err" -ErrorAction SilentlyContinue |
          Select-String 'ULE-VIRT|ULE-JITMETHOD' | Select-Object -First 1)
  $sigs = if ($sig) { $sig.Line.Substring(0, [Math]::Min(120, $sig.Line.Length)) } else { '' }
  "{0,-6} {1,6:N1}s deny='{2}' {3}" -f $verd, $sw.Elapsed.TotalSeconds, $Deny, $sigs
}
# `$n` would bind to the -N parameter; use a distinct name.
foreach ($ev in 'RAVA_BACKGROUND_JIT_QUEUE', 'RAVA_JIT_DENY') {
  [Environment]::SetEnvironmentVariable($ev, $null)
}

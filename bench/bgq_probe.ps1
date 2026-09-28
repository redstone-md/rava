# Where does raising the background-JIT queue depth start breaking the boot? Depth 4096 dies in
# 2.1s with ULE stack errors in Blocks/ButtonBlock/FireBlock, so more compilation exposes latent
# miscompiles. This walks the depth to find whether that is a wall or a gradient.
param([string]$Depths = '2,4,8,32')
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
foreach ($d in $Depths.Split(',')) {
  $env:RAVA_BACKGROUND_JIT_QUEUE = $d
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\bgqp_$d.log"
  Remove-Item -LiteralPath $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath 'target\pgo-fat\rava.exe' -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(240)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 250
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $dn = (Get-Content $out | Select-String 'Done \(([0-9.]+)s\)')
  $ok = if ($dn) { "Done=" + $dn.Matches[0].Groups[1].Value } else { "FAILED" }
  "depth={0,-5} wall={1,6:N2}s {2}" -f $d, $sw.Elapsed.TotalSeconds, $ok
}
[Environment]::SetEnvironmentVariable('RAVA_BACKGROUND_JIT_QUEUE', $null)

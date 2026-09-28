# Does the proven-top rollback let the background queue go deep without breaking the boot?
# Before the fix: depth 32 fails to start the server in 2.1s, 3/3.
param([string]$Depths = '32,32,8,8,8')
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
foreach ($d in $Depths.Split(',')) {
  $env:RAVA_BACKGROUND_JIT_QUEUE = $d
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $o = 'bench\statfix.log'
  Remove-Item -LiteralPath $o, "$o.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath 'target\statfix.exe' `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $o `
        -RedirectStandardError "$o.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(240)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 200
    if ($p.HasExited) { break }
    if (Test-Path $o) { if ((Get-Content $o -Tail 2) -match 'Done \(') { break } }
  }
  $sw.Stop()
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $dn = (Get-Content $o | Select-String 'Done \(([0-9.]+)s\)')
  $v = if ($dn) { 'BOOTED Done=' + $dn.Matches[0].Groups[1].Value } else { 'FAILED' }
  "depth={0,-4} {1,6:N1}s {2}" -f $d, $sw.Elapsed.TotalSeconds, $v
}
[Environment]::SetEnvironmentVariable('RAVA_BACKGROUND_JIT_QUEUE', $null)

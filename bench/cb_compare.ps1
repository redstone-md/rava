# WHAT gets more expensive in the serial prefix under RAVA_ARENA_LAMBDA? The lane is
# fixed and Done is now identical, so the flag's remaining +8.5 s lives entirely in
# blocks A+B and C. The callback census names the populations; running it with the flag
# off and on and diffing the rows says which one grew.
param([string]$Exe = 'target\divfold.exe')
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
foreach ($arm in @(@('off', ''), @('on', '1'))) {
  Remove-Item Env:RAVA_ARENA_LAMBDA -ErrorAction SilentlyContinue
  $env:RAVA_JIT = '1'; $env:RAVA_CALLBACK_STATS = '1'; $env:RAVA_JIT_STATS = '1'; $env:RAVA_JIT_STATS_MS = '25000'
  if ($arm[1]) { $env:RAVA_ARENA_LAMBDA = $arm[1] }
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\cbcmp_$($arm[0]).log"
  Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(220)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 500
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  $l = (Get-Content "$out.err" | Select-String 'ARENA-PROBE' | Select-Object -Last 1).Line
  Write-Host ("== {0}: {1}" -f $arm[0], (Get-Content $out -Tail 1))
  Write-Host ("   {0}" -f $l)
}
Remove-Item Env:RAVA_ARENA_LAMBDA, Env:RAVA_CALLBACK_STATS, Env:RAVA_JIT_STATS, Env:RAVA_JIT_STATS_MS -ErrorAction SilentlyContinue

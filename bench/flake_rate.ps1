# FLAKE RATE of the shipping binary. Row 44 saw 1 of 3 production boots die with the
# stale-receiver signature (a young object with no forwarding record reached as a
# receiver inside a compiled frame). One in three is from n=3; this measures the rate
# and captures the failing method each time, because a correctness flake in the binary
# we ship outranks any further performance work.
param([string]$Exe = 'target\pgo-fat\rava.exe', [int]$N = 6, [int]$TimeoutSec = 200)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$ok = 0; $bad = 0
for ($i = 1; $i -le $N; $i++) {
  Remove-Item Env:RAVA_JIT_THRESHOLD, Env:RAVA_ARENA_LAMBDA -ErrorAction SilentlyContinue
  $env:RAVA_JIT = '1'
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\flake_$i.log"
  Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
  $dl = [DateTime]::Now.AddSeconds($TimeoutSec)
  $done = $false
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 500
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { $done = $true; break } }
  }
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  if ($done) { $ok++; Write-Host "run $i OK" }
  else {
    $bad++
    $m = Get-Content "$out.err" -ErrorAction SilentlyContinue | Select-String 'ULE-JITMETHOD|uncaught' | Select-Object -First 1
    $where = if ($m) { $m.Line.Trim() } else { (Get-Content $out -Tail 1) }
    Write-Host ("run {0} FAIL  {1}" -f $i, $where.Substring(0, [Math]::Min(150, $where.Length)))
  }
}
Write-Host ("flake rate: {0}/{1} failed" -f $bad, $N)

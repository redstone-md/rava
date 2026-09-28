# COVERAGE: how many methods does each engine actually compile during this boot, and how many
# of those are OSR (a loop entered once, which rava cannot do at all -- ledger row 171)?
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
Remove-Item -LiteralPath 'bench\jvm_comp.txt','bench\jvmc1.log','bench\jvmc1.log.err' -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath 'java' -ArgumentList @(
      '-XX:TieredStopAtLevel=1', '-XX:+UnlockDiagnosticVMOptions',
      '-XX:+PrintCompilation', '-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput 'bench\jvm_comp.txt' `
      -RedirectStandardError 'bench\jvmc1.log.err' -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds(300)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 200
  if ($p.HasExited) { break }
  if (Test-Path 'bench\jvm_comp.txt') { if ((Get-Content 'bench\jvm_comp.txt' -Tail 3) -match 'Done \(') { break } }
}
$sw.Stop()
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
$L = Get-Content 'bench\jvm_comp.txt'
"JVM C1-only wall={0:N2}s" -f $sw.Elapsed.TotalSeconds
($L | Select-String 'Done \(') | ForEach-Object { $_.Line }
"compilation lines : {0}" -f ($L | Select-String '^\s+\d+\s+\d+\s').Count
"OSR compilations  : {0}" -f ($L | Select-String '% ').Count
"made-not-entrant  : {0}" -f ($L | Select-String 'made not entrant').Count

# Diagnostic: how many methods actually COMPILE at a given JIT threshold, and does
# the 4096-method `jit_cap` bind? Runs one boot per threshold with RAVA_JIT_STATS
# periodic dumps, then counts the methods with compile_ok>=1 in the LAST dump.
# Timing from these runs is NOT valid (the stats dump is heavy) — counts only.
param([string[]]$Thresholds = @('', '16'))

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

foreach ($th in $Thresholds) {
  $tag = if ($th) { "th$th" } else { 'thdef' }
  Remove-Item Env:RAVA_JIT_THRESHOLD -ErrorAction SilentlyContinue
  $env:RAVA_JIT = '1'; $env:RAVA_JIT_STATS = '1'; $env:RAVA_JIT_STATS_MS = '20000'
  if ($th) { $env:RAVA_JIT_THRESHOLD = $th }

  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\cap_$tag.log"; $err = "$out.err"
  Remove-Item $out, $err -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath 'target\release\rava.exe' `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError $err -PassThru -WindowStyle Hidden
  $p.ProcessorAffinity = [IntPtr]0x0FFF
  $dl = [DateTime]::Now.AddSeconds(300)
  while ([DateTime]::Now -lt $dl) {
    Start-Sleep -Milliseconds 500
    if ($p.HasExited) { break }
    if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
  }
  if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
  Write-Host ("ran $tag")
}
Remove-Item Env:RAVA_JIT_STATS, Env:RAVA_JIT_STATS_MS, Env:RAVA_JIT_THRESHOLD -ErrorAction SilentlyContinue

foreach ($th in $Thresholds) {
  $tag = if ($th) { "th$th" } else { 'thdef' }
  $e = "bench\cap_$tag.log.err"
  if (-not (Test-Path $e)) { continue }
  $c = Get-Content $e
  $dumps = $c | Select-String '\[JIT-STATS\] methods='
  $ok = ($c | Select-String 'compile_ok=[1-9]').Count
  $fail = ($c | Select-String 'compile_fail=[1-9]').Count
  $cached = ($c | Select-String 'cached_fail=[1-9]').Count
  $done = ($c | Select-String 'Done \(')
  Write-Host ("{0,-7} dumps={1} lines_compile_ok={2} lines_compile_fail={3} lines_cached_fail={4} | {5}" -f `
    $tag, $dumps.Count, $ok, $fail, $cached, ($dumps | Select-Object -Last 1).Line)
}

# THE ONLY LEGITIMATE INTERPRETER COMPARISON ON THE REAL WORKLOAD: rava with RAVA_JIT=0 against
# `java -Xint`. Both engines with their compiler off, same jar, same world, same box.
#
# ⚠ WHY THIS SCRIPT EXISTS. The per-block table in ledger row 223 compared rava WITH its JIT
# against `java -Xint` — our full engine against their crippled one. That is not "both without
# JIT" and it must not be read as an interpreter result. It says one useful thing only (in block D
# we lose even to a crippled JVM) and it is labelled as such.
#
# rava JIT=0 also turns off everything that depends on the JIT: the GIL-free ring lane arms only
# when `jit_enabled_env()` (gilfree_cf/rings.rs), and the direct-call caches have nothing to cache.
# So this measures the interpreter PLUS the natives PLUS the class loader, serially — which is
# exactly the shape `java -Xint` runs in.
#
#   bench\nojit_boot.ps1 -TimeoutS 2400
param([int]$TimeoutS = 2400, [string]$Exe = 'target\release\rava.exe', [switch]$Jvm)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
. "$PSScriptRoot\require_quiet.ps1"
Assert-QuietBox -Context 'the no-JIT boot'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"
$tag = if ($Jvm) { 'nojit_jvm' } else { 'nojit_rava' }
$log = "bench\$tag.log"
Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue

if ($Jvm) {
  $file = 'java'
  $argv = @('-Xint', '-cp', $cp, 'net.minecraft.server.Main', '--nogui')
} else {
  $file = $Exe
  $argv = @('-cp', $cp, 'net.minecraft.server.Main', '--nogui')
  $env:RAVA_JIT = '0'
  $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  # The two natives the product ships; they are not compiler features and turning them off would
  # measure a configuration nobody runs.
  $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
  Remove-Item Env:RAVA_JIT_TIER0 -ErrorAction SilentlyContinue
}
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $file -ArgumentList $argv -WorkingDirectory (Get-Location) `
      -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
$marks = @{}
$dl = [DateTime]::Now.AddSeconds($TimeoutS)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if (Test-Path $log) {
    foreach ($m in 'Environment:', 'recipes', 'Preparing level', 'Done \(') {
      if (-not $marks.ContainsKey($m) -and (Select-String -Path $log -Pattern $m -Quiet)) {
        $marks[$m] = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        "  {0,-16} t+{1}s" -f $m, $marks[$m]
      }
    }
  }
  if ($marks.ContainsKey('Done \(')) { break }
}
$sw.Stop()
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
Remove-Item Env:RAVA_JIT -ErrorAction SilentlyContinue
"{0}: wall={1}s reachedDone={2}" -f $tag, [math]::Round($sw.Elapsed.TotalSeconds, 1), $marks.ContainsKey('Done \(')
Select-String -Path $log -Pattern 'Done \(' | Select-Object -Last 1

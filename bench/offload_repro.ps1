# ONE OFFLOAD BOOT, STOPPED AT THE RELOAD VERDICT — the fast repro for the
# `RAVA_ASYNC_ANY_EXECUTOR` arc.
#
# ⭐The whole point is that this is NOT a 20-minute boot. Since row 421 the offloaded reload
# either fails or succeeds inside ~150 s, long before `Preparing level`, so the debug loop for
# this arc costs two minutes rather than half an hour. It stops on the datapack verdict, on
# `Preparing level` (the reload survived), or on process exit — whichever comes first — and
# then prints every `[THROW]`/`[NET-NPE]` head from the stderr trace.
param(
  [int]$TimeoutS = 420,
  [string]$Tag = 'thr',
  [switch]$NoFlag,         # control arm: identical run with the offload OFF
  [int]$NurseryKB = 0,     # oversized nursery => no minor GC in the window (GC discriminator)
  [hashtable]$Envs = @{},  # extra probes, e.g. @{RAVA_TRACE_REFMASK='1'}
  # ⚠Profile on `release-diag`, not on `release`: the product profile is `strip = "symbols"`
  # and the linker folds identical functions (/OPT:ICF), so a PC sample lands on whatever
  # symbol survived nearby — row 428 optimised a LABEL twice because of it.
  [string]$Exe = 'target/release/rava.exe',
  [switch]$Jit,            # PRODUCT configuration (compilers ON) instead of the -Xint-comparable one
  [switch]$Jvm,            # the reference engine, same jar/world/port
  [switch]$Trace           # arm RAVA_TRACE_THROW (costs a `format!` per call — never for timing)
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
& robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$env:RAVA_JIT = if ($Jit) { '1' } else { '0' }
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
if ($Jit) { $env:RAVA_JIT_TIER0 = '1' }
$env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
# ⛔**`RAVA_TRACE_THROW` IS NOT FREE AND MUST NOT BE THE DEFAULT.** It arms the named call stack,
# which `format!`s a method signature on EVERY interpreted call — this harness set it
# unconditionally, so every wall it ever printed carried that tax. Fine for the A/B it was built
# for (both arms paid it), fatal for any absolute number. Opt in with -Trace when chasing a throw.
if ($Trace) { $env:RAVA_TRACE_THROW = '1' } else { Remove-Item env:RAVA_TRACE_THROW -EA SilentlyContinue }
# Any env already set by the caller WINS over the two product defaults above: the offload arc
# needs to bisect them (the reload is JSON, so `RAVA_GSON_NATIVE` is a live suspect).
if ($env:GSON_OVERRIDE) { $env:RAVA_GSON_NATIVE = $env:GSON_OVERRIDE }
if ($NurseryKB -gt 0) { $env:RAVA_NURSERY_KB = "$NurseryKB" } else { Remove-Item env:RAVA_NURSERY_KB -EA SilentlyContinue }
if ($NoFlag) { Remove-Item env:RAVA_ASYNC_ANY_EXECUTOR -EA SilentlyContinue }
else { $env:RAVA_ASYNC_ANY_EXECUTOR = '1' }
foreach ($k in $Envs.Keys) { Set-Item "env:$k" $Envs[$k] }
$log = "bench\$Tag.log"; $err = "bench\$Tag.err"
Remove-Item $log, $err -EA SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$file = if ($Jvm) { 'java' } else { (Join-Path (Get-Location) $Exe) }
$argv = if ($Jvm) { @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') }
        else { @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') }
$p = Start-Process -FilePath $file `
      -ArgumentList $argv `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError $err `
      -PassThru -WindowStyle Hidden
$null = $p.Handle
try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
$dl = [DateTime]::Now.AddSeconds($TimeoutS)
while ([DateTime]::Now -lt $dl -and -not $p.HasExited) {
  Start-Sleep -Seconds 2
  # ⚠STOP ON A VERDICT, NOT ON A MILESTONE. `Preparing level` is printed ~19 minutes BEFORE
  # `Done` on the slow arm, so breaking there killed the control run before it could report the
  # only number the two arms are compared on.
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern "can't proceed with server load|Done \(" -Quiet)) { break }
}
$sw.Stop()
Start-Sleep -Seconds 1
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
$code = try { $p.ExitCode } catch { 'n/a' }
Write-Host ("arm={0} wall={1:N0}s exit={2}" -f $(if ($NoFlag) { 'OFF' } else { 'ON' }), $sw.Elapsed.TotalSeconds, $code)
$m = Select-String -Path $log -Pattern 'Done \(([\d.]+)s\)' | Select-Object -Last 1
Write-Host ("Done={0}" -f $(if ($m) { $m.Matches.Groups[1].Value } else { 'NEVER REACHED' }))
Write-Host '--- stdout tail ---'; Get-Content $log -Tail 6
Write-Host '--- throw heads ---'
Select-String -Path $err -Pattern '^\[(THROW|NET-NPE)\]' | ForEach-Object { $_.Line }

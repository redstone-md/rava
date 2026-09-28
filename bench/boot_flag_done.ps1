# ONE COMPILERS-OFF BOOT, PAIRED ON A FLAG, REPORTING `Done` AND WALL.
#
# ⭐**The benchmark this project is actually judged by.** Rows 367-372 established that the
# compilers-off boot's 29.3x gap is 91% ONE phase — the stronghold ring search, which the JVM
# never runs during boot because MC hands it to `Util.backgroundExecutor()` and never observes
# the future, while rava runs it inline. This harness exists to test flags against that.
#
# ⚠A boot is ~25 minutes with compilers off, so this is n=1 per arm by necessity: it answers
# "did the phase move" (a several-fold change), never "is this 3% faster".
param(
  [Parameter(Mandatory = $true)][string]$Flag,
  [string]$Value = '1',
  [int]$TimeoutS = 3000
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())

function BootOnce([string]$tag, [bool]$set) {
  & robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  $env:RAVA_JIT = '0'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
  if ($env:BOOT_NURSERY_KB) { $env:RAVA_NURSERY_KB = $env:BOOT_NURSERY_KB }
  if ($set) { Set-Item "env:$Flag" $Value } else { Remove-Item "env:$Flag" -EA SilentlyContinue }
  $log = "bench\bootflag_$tag.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath (Join-Path (Get-Location) 'target\release\rava.exe') `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
        -PassThru -WindowStyle Hidden
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $dl = [DateTime]::Now.AddSeconds($TimeoutS)
  while ([DateTime]::Now -lt $dl -and -not $p.HasExited) {
    Start-Sleep -Seconds 2
    if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { break }
  }
  $sw.Stop()
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  $m = Select-String -Path $log -Pattern 'Done \(([\d.]+)s\)' | Select-Object -Last 1
  $done = if ($m) { $m.Matches.Groups[1].Value } else { 'NEVER REACHED' }
  $last = if (Test-Path $log) { (Get-Content $log -Tail 1) } else { '' }
  # ⚠**REPORT THE EXIT CODE.** A boot that ends early ends for a REASON, and the first version of
  # this harness threw that reason away — an arm that exits at 68 s and one that is killed at the
  # deadline printed the same thing. 0xC0000005 is an access violation; 3 is our own hard exit.
  $code = try { $p.ExitCode } catch { 'n/a' }
  Write-Host ("  {0,-8} Done={1,-12} exit={2,-12} wall={3:N0}s   last: {4}" -f $tag, $done, $code, $sw.Elapsed.TotalSeconds, $last)
}
Write-Host "compilers-off boot, flag $Flag=$Value"
BootOnce 'OFF' $false
BootOnce 'ON'  $true

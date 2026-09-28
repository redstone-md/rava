# HUNT AN INTERMITTENT FAULT UNDER THE CONDITIONS IT ACTUALLY APPEARS IN.
#
# The 0xC0000005 of rows 418-419 shows up under `boot_flag_done.ps1` (affinity-pinned,
# Start-Process, redirected) and not on three direct shell runs of the same code. So reproduce
# THOSE conditions, with the symbol-bearing `release-diag` binary so the crash handler can name
# the faulting instruction rather than printing a bare address.
param([int]$Runs = 4, [int]$TimeoutS = 220, [string]$Exe = 'target\release-diag\rava.exe')
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT='0'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'
for ($i = 1; $i -le $Runs; $i++) {
  & robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  $log = "bench\crashhunt.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $p = Start-Process -FilePath (Join-Path (Get-Location) $Exe) `
        -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
        -PassThru -WindowStyle Hidden
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  if (-not $p.WaitForExit($TimeoutS * 1000)) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue; Write-Host "  run $i : survived $TimeoutS s"; continue }
  $code = try { $p.ExitCode } catch { 'n/a' }
  Write-Host "  run $i : EXITED code=$code"
  if (Test-Path "$log.err") { Get-Content "$log.err" | Select-String -Pattern 'CRASH' -Context 0,2 | ForEach-Object { Write-Host "    $_" } }
  if ($code -eq -1073741819) { Write-Host "  --> caught the access violation on run $i"; break }
}

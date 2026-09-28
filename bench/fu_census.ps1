# WHICH CLASSES does the compiled-code collection fast path turn away?
#
# `fu_fastpath` admits six receiver classes and refuses everything else; the lane probe reports
# 7.9 M refusals a boot as one number (`fu-why: ref_class_nomatch`). The per-class breakdown is
# already instrumented — `fu_fastpath` notes the class name under the `FU:` prefix when the
# back-edge census is armed — but it needs BOTH flags, and the reporter lives in the lane probe.
#
#   bench\fu_census.ps1
param([int]$ProbeMs = 5000, [string]$Exe = 'target\release\rava.exe', [int]$TimeoutS = 300)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'; $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$env:RAVA_CALLBACK_STATS = '1'
$env:RAVA_BACKEDGE_CENSUS = '1'
$env:RAVA_LANE_PROBE = "$ProbeMs"
$log = 'bench\fu_census.log'
Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
      -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds($TimeoutS)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 300
  if ($p.HasExited) { break }
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) {
    Start-Sleep -Seconds ([math]::Ceiling($ProbeMs / 1000) + 1)
    break
  }
}
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
foreach ($k in 'RAVA_CALLBACK_STATS', 'RAVA_BACKEDGE_CENSUS', 'RAVA_LANE_PROBE') {
  Remove-Item "env:$k" -ErrorAction SilentlyContinue
}
"{0}s -> {1}.err" -f [math]::Round($sw.Elapsed.TotalSeconds, 1), $log

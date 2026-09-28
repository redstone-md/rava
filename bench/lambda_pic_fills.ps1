# DID `RAVA_LAMBDA_PIC` ENGAGE ON THE REAL BOOT AT ALL?
#
# A null wall delta cannot distinguish two very different answers: "the mechanism engaged and was
# worth nothing" and "the mechanism never engaged". Only the second is a statement about the BOOT
# rather than about the mechanism, and only the fill count separates them.
#
# `RAVA_JIT_PIC_PROBE` reports the dispatch census including lambda-PIC fills (`leaf=` / `deep=`).
# Zero fills with the flag ON means every candidate was refused - the flag emitted its select at
# every refpic site, paid for it, and never served a call.
#
# ⚠ RAVA_LANE_PROBE is armed too, and NOT for the lane: picprobe::dump() fires from the CLI clean
# exit, which a boot measured to Done and then killed never reaches. The lane probe carries the
# only other call to it. Someone hit this before and moved the dump there; the comment at
# lane_probe.rs records it.
#
# Context (2026-07-29 per-site attribution): the boot's PIC-miss time is dominated by
# IndirectMerger @cp48 and DiscreteCubeMerger @cp41, both calling IndexConsumer.merge with what is
# almost certainly a CAPTURING closure. RAVA_LAMBDA_PIC serves only the NON-capturing arm, so the
# predicted answer is "few or no fills at those sites".
param(
  [int]$TimeoutS = 500,
  [string]$Exe = 'D:\code\rava\target\release\rava.exe'
)
$root='D:\code\rava'
if(-not (Test-Path $Exe)){ Write-Host "missing $Exe"; exit 1 }
$cp='out;'+((Get-Content "$root\bench\mc-cp.txt" -Raw).Trim())
foreach($k in 'RAVA_PROFILE_WALL','RAVA_PROFILE_PC','RAVA_BRIDGE_STAT','RAVA_CELL_CENSUS',
              'RAVA_COMPILE_TIME','RAVA_ARENA_LAMBDA'){ Remove-Item "env:$k" -EA SilentlyContinue }
$e=@{RAVA_JIT='1';RAVA_PROC_CAP_MB='4096';RAVA_HARD_MB='3584';RAVA_GSON_NATIVE='1';
     RAVA_M3_BITSTORAGE='1';RAVA_JIT_TIER0='1';RAVA_LAMBDA_PIC='1';RAVA_JIT_PIC_PROBE='1';RAVA_LANE_PROBE='1'}
foreach($k in $e.Keys){ Set-Item "env:$k" $e[$k] }
robocopy "$root\world_pristine" "$root\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$root\bench\lampic.log"
$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $Exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
    -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
$p.ProcessorAffinity=[IntPtr]0x0FFF
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){break}
  Start-Sleep -Milliseconds 400
  if((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)){break}
}
Write-Host ("wall=" + [math]::Round($sw.Elapsed.TotalSeconds,1) + "s  (probe run - NOT a speed measurement)")
Start-Sleep -Milliseconds 1500
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
try { $p.WaitForExit(5000) | Out-Null } catch {}
$done = Select-String -Path $log -Pattern 'Done \(' | Select-Object -First 1
if($done){ Write-Host $done.Line } else { Write-Host "*** no Done line" }
Write-Host "--- dispatch census / lambda-PIC fills ---"
Get-Content "$log.err" -EA SilentlyContinue |
  Select-String -Pattern 'PIC|lambda|LAMBDA|fill|directable|dispatch' |
  Select-Object -Last 30 | ForEach-Object { $_.Line }

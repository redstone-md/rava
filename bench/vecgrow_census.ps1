# Vec-growth census over the A+B window (RAVA_VECGROW_STAT).
# Counts only - no wall is reported or implied. Boots the server, polls the log for
# the `Environment:` marker, records the elapsed seconds at which it appeared, and
# kills the run. The census snapshots (stderr, stamped `t=`) are then read at the
# snapshot nearest that stamp, so the numbers belong to blocks A+B.
param(
  [string]$Exe = '',
  [int]$TimeoutS = 400,
  [string]$Marker = 'Environment:',
  [string]$Tag = 'vecgrow',
  # Comma-separated env flags to set for this run (the census is a COUNT probe,
  # so an A/B here is a comparison of counts, never of walls).
  [string]$Flag = ''
)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = 'D:\code\rava'
if (-not $Exe) { $Exe = Join-Path $here '..\target\release\rava.exe' }
# mc-cp.txt is untracked and lives in the main checkout's bench dir.
$cpf = Join-Path $here 'mc-cp.txt'
if (-not (Test-Path $cpf)) { $cpf = Join-Path $root 'bench\mc-cp.txt' }
$cp = 'out;' + ((Get-Content $cpf -Raw).Trim())

foreach($k in @('RAVA_JIT','RAVA_JIT_TIER0','RAVA_GSON_NATIVE','RAVA_M3_BITSTORAGE')){ Set-Item "env:$k" '1' }
$env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_VECGROW_STAT='1'
$setflags = @($Flag -split ',' | Where-Object { $_ })
foreach($f in $setflags){ Set-Item "env:$f" '1' }

$log = Join-Path $here "$Tag.log"
$err = Join-Path $here "$Tag.err"
foreach($f in @($log,$err)){ if(Test-Path $f){ Remove-Item $f -Force } }

$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $Exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
    -WorkingDirectory $root -RedirectStandardOutput $log -RedirectStandardError $err -PassThru
try { $p.ProcessorAffinity=[IntPtr]0x0FFF } catch {}
$hit=$false; $at=0
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  $exited = $p.HasExited
  if((Test-Path $log) -and (Select-String -Path $log -Pattern $Marker -Quiet)){
    $hit=$true; $at=[math]::Round($sw.Elapsed.TotalSeconds,1); break
  }
  if($exited){ break }
  Start-Sleep -Milliseconds 200
}
# Let one more snapshot land past the marker so the window edge is bracketed.
Start-Sleep -Milliseconds 2200
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
$env:RAVA_VECGROW_STAT=$null
foreach($f in $setflags){ Set-Item "env:$f" $null }
Write-Host ("marker '{0}' reached={1} at t={2}s" -f $Marker,$hit,$at)
Write-Host ("census: {0}" -f $err)

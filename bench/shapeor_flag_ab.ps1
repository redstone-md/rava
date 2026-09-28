# Interleaved Rava-vs-Rava A/B of ONE env flag on the ShapeOr probe (the VoxelShape CSG
# concentration inside Bootstrap.bootStrap). Same P-core affinity + High-Performance plan
# as shapeor_ab.ps1; alternates off/on so drift hits both arms.
#   .\shapeor_flag_ab.ps1 -Flag RAVA_LAMBDA_PIC -Seq off,on,on,off -Passes 40
param(
  [Parameter(Mandatory=$true)][string]$Flag,
  [string]$Seq = 'off,on,on,off',
  [int]$Passes = 40,
  [int]$Warmup = 10
)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$mask = 0x0FFF
$exe  = Join-Path $here '..\target\release\rava.exe'
$cp   = 'out;' + ((Get-Content (Join-Path $here 'mc-cp.txt') -Raw).Trim())

$orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
powercfg /setactive SCHEME_MIN | Out-Null
$env:RAVA_JIT='1'; $env:RAVA_JIT_TIER0='1'; $env:RAVA_GSON_NATIVE='1'
$env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'

$off = @(); $on = @(); $sums = @()
$i = 0
foreach ($cfg in ($Seq -split ',')) {
  $i++
  if ($cfg -eq 'on') { Set-Item "env:$Flag" '1' } else { Set-Item "env:$Flag" $null }
  $log = Join-Path $here "shapeor_flag_$($Flag)_$($i)_$cfg.log"
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'ShapeOr', "$Passes", "$Warmup") `
        -WorkingDirectory $here -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  try { $p.ProcessorAffinity = [IntPtr]$mask } catch {}
  $p.WaitForExit(1800000) | Out-Null
  $nsM = (Select-String $log -Pattern 'HOTNS=(\d+)' | Select-Object -First 1)
  $sumM = (Select-String $log -Pattern 'SUM=([0-9a-f]+)' | Select-Object -First 1)
  if (-not $nsM) { Write-Host "run$i ${cfg}: NO OUTPUT (see $log.err)"; continue }
  $ms = [double]$nsM.Matches.Groups[1].Value / 1e6
  $sum = $sumM.Matches.Groups[1].Value
  $sums += $sum
  if ($cfg -eq 'on') { $on += $ms } else { $off += $ms }
  Write-Host ("run{0} {1}: {2:N0}ms sum={3}" -f $i, $cfg, $ms, $sum)
}
Set-Item "env:$Flag" $null
powercfg /setactive $orig | Out-Null

function Med($a) { $s = @($a | Sort-Object); return $s[[int]([math]::Floor($s.Count/2))] }
if ($off.Count -and $on.Count) {
  $mo = Med $off; $mn = Med $on
  Write-Host ("== {0}: off={1:N0}ms on={2:N0}ms  delta={3:N1}% ==" -f $Flag, $mo, $mn, (100.0*($mn-$mo)/$mo))
}
$d = @($sums | Sort-Object -Unique)
if ($d.Count -eq 1) { Write-Host "GATE: SUM bit-exact across both arms = $($d[0])" }
else { Write-Host "GATE: SUM MISMATCH -> $($d -join ' ')" }

# STEP 6 PAYOFF PILOT — measure the value-model endgame's prize on a program whose
# object graph is 100% arena-born (FieldsRef: plain user classes, ref-field hot loop).
#   A = current tagged value model (per-deref 2-bit discrimination)
#   B = RAVA_JIT_BARE_REF_LOAD (bare load/store — what step 6 emits after retirement)
#   J = JVM reference
# Reports median HOTNS of $Runs runs each. P-core affinity + High-Performance plan.
param([int]$Runs = 5, [string]$Bench = 'FieldsRef', [string]$BenchArgs = '')
$root='D:\code\rava'; $here="$root\bench"; $exe="$root\target\release\rava.exe"
$java='C:\Program Files\Eclipse Adoptium\jdk-25.0.3.9-hotspot\bin\java.exe'
$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'; $env:RAVA_JIT_TIER0='1'
$orig=(powercfg /getactivescheme); if($orig -match '([0-9a-f-]{36})'){$orig=$Matches[1]}
powercfg /setactive SCHEME_MIN | Out-Null
function Median($xs){ $s=@($xs | Sort-Object); if($s.Count -eq 0){return 0}; $s[[int]($s.Count/2)] }
function RunOne($file,$argline,$log){
  $al=@('-cp',"$here\out",$Bench); if($BenchArgs){ $al += $BenchArgs.Split(' ') }
  if($argline){ $al = $argline + $al }
  $p=Start-Process -FilePath $file -ArgumentList $al -WorkingDirectory $here -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  $p.ProcessorAffinity=[IntPtr]0x0FFF; $p.WaitForExit(900000) | Out-Null
  $m=(Select-String $log -Pattern 'HOTNS=(\d+)' | Select-Object -First 1)
  $s=(Select-String $log -Pattern 'SUM=(-?\d+)' | Select-Object -First 1)
  [pscustomobject]@{ ns = if($m){[int64]$m.Matches.Groups[1].Value}else{-1}; sum = if($s){$s.Matches.Groups[1].Value}else{'NONE'} }
}
$res=@{}
foreach($cfg in @('tagged','bare','jvm')){
  $ns=@(); $sums=@()
  foreach($i in 1..$Runs){
    if($cfg -eq 'bare'){ $env:RAVA_JIT_BARE_REF_LOAD='1' } else { $env:RAVA_JIT_BARE_REF_LOAD=$null }
    $log="$here\pilot_$($Bench)_$($cfg)_$i.log"
    $r = if($cfg -eq 'jvm'){ RunOne $java $null $log } else { RunOne $exe $null $log }
    $ns += $r.ns; $sums += $r.sum
  }
  $res[$cfg]=Median $ns
  Write-Host ("{0,-7} median HOTNS={1,15:N0}  runs={2}  sums={3}" -f $cfg,$res[$cfg],($ns -join ','),(($sums | Select-Object -Unique) -join '|'))
}
$env:RAVA_JIT_BARE_REF_LOAD=$null
powercfg /setactive $orig | Out-Null
if($res['bare'] -gt 0 -and $res['tagged'] -gt 0){
  Write-Host ("PRIZE: bare vs tagged = {0:N2}x   |   RAVA(bare) vs JVM = {1:N2}x   |   RAVA(tagged) vs JVM = {2:N2}x" -f `
    ($res['tagged']/$res['bare']), ($res['bare']/[double]$res['jvm']), ($res['tagged']/[double]$res['jvm']))
}
"PILOT DONE" | Out-File "$here\bare_ref_pilot.done" -Encoding utf8

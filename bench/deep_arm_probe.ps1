# Does the machine-code DEEP arm serve the ring's hot noise chain at all?
#
# RingScan is the ring workload (bit-exact gate 37539c6c375d0c3) and is the workload
# measured ~61x slower than the JVM. RAVA_JIT_STATS says its innermost body,
# ImprovedNoise.sampleAndLerp, is COMPILED yet its compiled entry runs once, while its
# leaves p(I)I and gradDot(IDDD)D are entered from the INTERPRETER 15,946 / 8,888
# times — the signature of sampleAndLerp executing interpreted.
#
# Three points, same binary:
#   default        deep arm on, 64-slot call buffer
#   NO_IC_DEEP     deep arm off entirely
#   NO_CALLBUF     buffer back to 32 (deep service limited to small frames)
# If the last two do not slow RingScan down, the deep arm is not serving this chain
# and the 62-slot sampleAndLerp is on the general path for some other reason.
param([string]$Exe = 'target\release\rava.exe', [int]$Reps = 2)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'

foreach ($case in @(
  @{ tag = 'default';    env = @{} },
  @{ tag = 'noicdeep';   env = @{ RAVA_NO_IC_DEEP = '1' } },
  @{ tag = 'nocallbuf';  env = @{ RAVA_NO_CALLBUF_WIDE = '1' } }
)) {
  Remove-Item Env:RAVA_NO_IC_DEEP, Env:RAVA_NO_CALLBUF_WIDE -ErrorAction SilentlyContinue
  foreach ($k in $case.env.Keys) { Set-Item -Path "Env:$k" -Value $case.env[$k] }
  $times = @()
  for ($i = 1; $i -le $Reps; $i++) {
    $out = "bench\deeparm_$($case.tag)_$i.log"
    Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
    $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'RingScan') `
          -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
          -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
    $p.ProcessorAffinity = [IntPtr]0x0FFF
    $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::High
    if (-not $p.WaitForExit(600000)) { $p.Kill(); $times += 'TIMEOUT'; continue }
    $line = Get-Content $out -Tail 1
    if ($line -match 'RINGS_MS=(\d+).*HASH=([0-9a-f]+)') { $times += "$($Matches[1])ms/$($Matches[2])" }
    else { $times += "FAIL($line)" }
  }
  Write-Host ("{0,-11} {1}" -f $case.tag, ($times -join '  '))
}
Remove-Item Env:RAVA_NO_IC_DEEP, Env:RAVA_NO_CALLBUF_WIDE -ErrorAction SilentlyContinue

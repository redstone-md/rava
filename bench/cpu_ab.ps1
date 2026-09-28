# PAIRED A/B ON **CPU TIME AND CYCLES**, NOT WALL.
#
# ⭐**THE MEASUREMENT FLOOR IS THE BLOCKER, NOT THE IDEAS** (ledger row 404). Every remaining
# frame-free increment prices at 1-3%, and this box's WALL arm-spread on a fixed binary is ±10%
# and has reached 1.9× — so the ledger has been unable to accept or reject any of them. The box
# carries ~29% background load from the user's own applications, which must not be touched.
#
# Wall time charges a run for every cycle some other process stole. CPU time does not: the OS
# accounts only the cycles this process actually ran. QueryProcessCycleTime goes further and
# counts CYCLES, which is immune to the frequency scaling and E-core migration that ledger row
# "bootbench noise infra" measured at 2.1× on its own.
#
# ⚠**VALIDATE IT AGAINST ITSELF FIRST** (`-A x.exe -B x.exe`). An instrument that cannot read
# zero on two copies of one binary cannot read anything else either - this tree has been burned
# by an A/B whose own noise floor was never established.
param(
  [Parameter(Mandatory = $true)][string]$A,
  [Parameter(Mandatory = $true)][string]$B,
  [Parameter(Mandatory = $true)][string]$Prog,
  [string[]]$ProgArgs = @(),
  [Parameter(Mandatory = $true)][string]$Expect,
  [int]$Pairs = 5,
  [string]$Cp = 'out',
  [switch]$Mc
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
if ($Mc) { $Cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim()) }
$env:RAVA_JIT = '0'; $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$env:RAVA_NURSERY_KB = '1048576'

# QueryProcessCycleTime: cycles actually executed by the process, across all its threads.
if (-not ('Win32Cycles' -as [type])) {
  Add-Type -Namespace '' -Name Win32Cycles -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError=true)]
public static extern bool QueryProcessCycleTime(IntPtr h, out ulong cycles);
'@
}

function One([string]$exe, [string]$tag) {
  $log = "bench\cpuab_$tag.log"
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $argv = @('-cp', $Cp, $Prog) + $ProgArgs
  $p = Start-Process -FilePath $exe -ArgumentList $argv -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru -WindowStyle Hidden
  # PowerShell 5.1 leaves the process object half-initialised until the handle is touched.
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $cyc = 0
  # Sample the cycle counter just before exit: the handle dies with the process.
  while (-not $p.HasExited) {
    Start-Sleep -Milliseconds 200
    $c = [uint64]0
    try { if ([Win32Cycles]::QueryProcessCycleTime($p.Handle, [ref]$c)) { if ($c -gt $cyc) { $cyc = $c } } } catch {}
  }
  $cpu = $p.TotalProcessorTime.TotalMilliseconds
  $text = if (Test-Path $log) { Get-Content $log -Raw } else { '' }
  if ($null -eq $text -or -not $text.Contains($Expect)) {
    Write-Host ("  {0}: DISCARDED (output lacks '{1}')" -f $tag, $Expect); return $null
  }
  return [pscustomobject]@{ cpu = $cpu; cyc = $cyc }
}

$ra = @(); $rb = @()
for ($i = 0; $i -lt $Pairs; $i++) {
  $order = if ($i % 2 -eq 0) { @('A', 'B') } else { @('B', 'A') }
  foreach ($s in $order) {
    $v = if ($s -eq 'A') { One $A 'a' } else { One $B 'b' }
    if ($null -ne $v) { if ($s -eq 'A') { $ra += $v } else { $rb += $v } }
  }
}
if ($ra.Count -eq 0 -or $rb.Count -eq 0) { Write-Host 'no valid pairs'; exit 1 }
function Report($name, $av, $bv) {
  $am = ($av | Measure-Object -Minimum).Minimum; $bm = ($bv | Measure-Object -Minimum).Minimum
  $ax = ($av | Measure-Object -Maximum).Maximum; $bx = ($bv | Measure-Object -Maximum).Maximum
  $verdict = if ($ax -lt $bm) { 'every A below every B (DISJOINT)' }
             elseif ($am -gt $bx) { 'every A above every B (DISJOINT, against)' }
             else { 'arms overlap' }
  $spreadA = if ($am -gt 0) { ($ax - $am) * 100 / $am } else { 0 }
  Write-Host ("  {0,-7} A {1}" -f $name, (($av | ForEach-Object { '{0:N0}' -f $_ }) -join ' / '))
  Write-Host ("  {0,-7} B {1}" -f '', (($bv | ForEach-Object { '{0:N0}' -f $_ }) -join ' / '))
  Write-Host ("  {0,-7} A vs B {1:N2}%   A-arm spread {2:N1}%   {3}" -f '', (($am - $bm) * 100 / $bm), $spreadA, $verdict)
}
Write-Host "$Prog  (CPU time and cycles; wall is deliberately not reported)"
Report 'cpu_ms' ($ra.cpu) ($rb.cpu)
Report 'cycles' ($ra.cyc) ($rb.cyc)

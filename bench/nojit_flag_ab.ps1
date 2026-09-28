# SAME-BINARY FLAG A/B ON THE COMPILERS-OFF BOOT — the comparison this project is judged on.
#
# WHY IT EXISTS (ledger row 351). A change measured at -3.19% on `interp_gate.ps1` read
# wall +2.5% / `Done` -1.2% on two single boots taken across commits — signs disagreeing, i.e. a
# NON-measurement, not a refutation. The boot's floor is ~4%, so a 3% effect needs alternation and
# several pairs. This does that with the change's own opt-out, which is a same-binary comparison
# and the strongest form in this tree's order of trust.
#
# ⚠A no-compiler boot is ~23 MINUTES. `-Pairs 2` is ~1.5 h; `-Pairs 3` is ~2.3 h.
#
#   bench\nojit_flag_ab.ps1 -Flag RAVA_NO_HOT_GETFIELD -Pairs 2
param(
  [Parameter(Mandatory = $true)][string]$Flag,
  [int]$Pairs = 2,
  [int]$TimeoutS = 2400,
  [string]$Exe = 'target\release\rava.exe'
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"

function One([string]$side) {
  & robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  $env:RAVA_JIT = '0'
  $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
  $env:RAVA_JIT_TIER0 = $null
  # "on" = the feature ON = the OPT-OUT unset. Spelled from the feature's side so the report reads
  # the way the change does.
  # ⚠**`Remove-Item`, NOT `Set-Item ... $null`.** Setting an environment variable to `$null` through
  # `Set-Item` does not remove it — it errors, leaving whatever the PREVIOUS run set, or leaves an
  # EMPTY variable, and Rust's `var_os` returns `Some("")` for that, which every opt-out gate in this
  # tree reads as "set". Either way both arms would run the same configuration. That is exactly how
  # `RAVA_DISPATCH_TABLE` cost this tree a session (row 303); the rule is to make the arms provably
  # different before spending the hour.
  if ($side -eq 'on') { Remove-Item "env:$Flag" -ErrorAction SilentlyContinue } else { Set-Item "env:$Flag" '1' }
  $log = "bench\nojitab_$side.log"
  Remove-Item -LiteralPath $log, "$log.err" -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
        -PassThru -WindowStyle Hidden
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $dl = [DateTime]::Now.AddSeconds($TimeoutS)
  while ([DateTime]::Now -lt $dl -and -not $p.HasExited) {
    Start-Sleep -Milliseconds 500
    if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { break }
  }
  $sw.Stop()
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
  $wall = [math]::Round($sw.Elapsed.TotalSeconds, 1)
  $done = 0.0
  $m = Select-String -Path $log -Pattern 'Done \(([\d.]+)s\)' | Select-Object -Last 1
  if ($m) { $done = [double]$m.Matches.Groups[1].Value }
  Write-Host ("{0}`twall={1}`tDone={2}" -f $side, $wall, $done)
  [pscustomobject]@{ side = $side; wall = $wall; done = $done }
}

$r = @()
foreach ($i in 1..$Pairs) {
  # ABBA: the order flips each pair, so a monotone drift cancels within the pair-of-pairs.
  $order = if ($i % 2 -eq 1) { @('off', 'on') } else { @('on', 'off') }
  foreach ($s in $order) { $r += One $s }
}
Write-Host ""
foreach ($s in 'off', 'on') {
  $g = $r | Where-Object { $_.side -eq $s }
  Write-Host ("{0}: wall min={1} runs={2} | Done min={3} runs={4}" -f `
    $s, ($g.wall | Measure-Object -Minimum).Minimum, ($g.wall -join '/'), `
    ($g.done | Measure-Object -Minimum).Minimum, ($g.done -join '/'))
}
$wo = @(($r | Where-Object { $_.side -eq 'off' }).wall); $wn = @(($r | Where-Object { $_.side -eq 'on' }).wall)
$do_ = @(($r | Where-Object { $_.side -eq 'off' }).done); $dn = @(($r | Where-Object { $_.side -eq 'on' }).done)
$mo = ($wo | Measure-Object -Minimum).Minimum; $mn = ($wn | Measure-Object -Minimum).Minimum
Write-Host ("wall min-of-N: {0:P2}   Done min-of-N: {1:P2}   (negative = feature ON faster)" -f `
  (($mn - $mo) / $mo), ((($dn | Measure-Object -Minimum).Minimum - ($do_ | Measure-Object -Minimum).Minimum) / ($do_ | Measure-Object -Minimum).Minimum))
# Disagreeing signs are how this ledger recognises a non-measurement (rows 314, 351).
$dw = @(); for ($k = 0; $k -lt [Math]::Min($wo.Count, $wn.Count); $k++) { $dw += ($wn[$k] - $wo[$k]) }
$dd = @(); for ($k = 0; $k -lt [Math]::Min($do_.Count, $dn.Count); $k++) { $dd += ($dn[$k] - $do_[$k]) }
Write-Host ("paired wall: {0}   favouring ON: {1}/{2}" -f ($dw -join '  '), (($dw | Where-Object { $_ -lt 0 }).Count), $dw.Count)
Write-Host ("paired Done: {0}   favouring ON: {1}/{2}" -f ($dd -join '  '), (($dd | Where-Object { $_ -lt 0 }).Count), $dd.Count)

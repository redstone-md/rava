# INTERLEAVED SAME-BINARY BOOT A/B — the only boot instrument this project trusts for a
# few-percent claim (order of trust: same-binary FLAG A/B > two-binary > across-commit).
#
# ⚠A boot is ~2 minutes now, which is the whole reason this is affordable: before the async
# offload landed it was 24, so a 4-run ABBA cost an hour and a half and nobody ran one. Reports
# each arm's wall and the min-of-N per arm, plus whether the arms are DISJOINT.
param([string]$Flag = 'RAVA_NO_BIRTH_MEMO', [int]$Rounds = 2, [int]$TimeoutS = 400,
      [switch]$Jit,    # PRODUCT configuration (compilers on) — the one users run
      [string]$Value = '1')  # for a flag that takes a number, not a presence
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$exe = Join-Path (Get-Location) 'target\release\rava.exe'
$script:seq = 0
function BootOnce([bool]$set) {
  $script:seq++
  & robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
  $env:RAVA_JIT = if ($Jit) { '1' } else { '0' }
  # ⛔**DO NOT set RAVA_OFFLOAD_FIRST here.** `bench/jvm_cpu.ps1` carried the same line and it
  # made every "product" number it printed a number for that FLAG rather than the default — the
  # error is recorded in the ledger (row 452) and this harness had the same one. Re-measured
  # today the flag is inert on a boot (33.6 s against 31.2 s default), so its only remaining
  # effect here was to measure a configuration nobody runs.
  if ($Jit) { $env:RAVA_JIT_TIER0 = '1'; Remove-Item env:RAVA_OFFLOAD_FIRST -EA SilentlyContinue }
  $env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
  $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
  if ($set) { Set-Item "env:$Flag" $Value } else { Remove-Item "env:$Flag" -EA SilentlyContinue }
  $log = 'bench\abflag.log'
  Remove-Item $log, "$log.err" -EA SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $exe -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
        -PassThru -WindowStyle Hidden
  $null = $p.Handle
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $dl = [DateTime]::Now.AddSeconds($TimeoutS)
  while ([DateTime]::Now -lt $dl -and -not $p.HasExited) {
    Start-Sleep -Milliseconds 500
    if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { break }
  }
  $sw.Stop()
  # ⭐**CPU TIME, NOT WALL.** The boot's wall floor is +/-8% (row 437: two ABBA rounds produced a
  # DISJOINT -8.52% that six boots reduced to -1.08% overlapping, and a default shipped on it).
  # Wall carries every scheduler hiccup, every background service and the robocopy that preceded
  # it; CPU time carries the work. `TotalProcessorTime` must be read BEFORE the kill — it is
  # unavailable on an exited process — and it sums every thread, which is what we want now that
  # the boot genuinely uses more than one.
  $cpu = try { $p.Refresh(); $p.TotalProcessorTime.TotalSeconds } catch { 0 }
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
  [pscustomobject]@{ Wall = [math]::Round($sw.Elapsed.TotalSeconds, 1); Cpu = [math]::Round($cpu, 1) ; Seq=$script:seq}
}
# ⚠⚠**THE NULL TEST FAILS ON THIS BOX, AND IT FAILS IN BOTH DIRECTIONS.** Run against a flag that
# does not exist — two identical arms — this harness returned:
#
#   null test 1:  SET faster by 7.34%,  disjoint=True
#   null test 2:  SET faster by 7.82%,  disjoint=True   (after adding the warm-up + median below)
#   null test 3:  SET SLOWER by 7.45% (min) / 4.60% (median),  disjoint=False
#
# **The sign flipped between consecutive null tests, and one of them claimed disjoint.** So
# `disjoint` is not a safety net at this magnitude, and no A/B smaller than about 10% can be
# trusted here until the box settles. The structure is sound — the order log below shows the
# alternation working and the warm-up discarded — the variance is the machine.
#
# Two mitigations are in: burn one boot before measuring, and report the MEDIAN beside the min so
# a single lucky run cannot carry a verdict. Neither is sufficient; read the null test first.
Write-Host "  warm-up boot (discarded)..."
$null = BootOnce $false

$on = @(); $off = @()
for ($r = 1; $r -le $Rounds; $r++) {
  # ABBA within each round, so a monotonic drift in machine state cancels instead of
  # landing entirely on one arm.
  if ($r % 2 -eq 1) { $off += BootOnce $false; $on += BootOnce $true }
  else              { $on += BootOnce $true;  $off += BootOnce $false }
  Write-Host ("  round {0}: UNSET wall={1} cpu={2}   SET wall={3} cpu={4}" -f `
    $r, $off[-1].Wall, $off[-1].Cpu, $on[-1].Wall, $on[-1].Cpu)
}
$offCpu = $off | ForEach-Object { $_.Cpu }
$onCpu  = $on  | ForEach-Object { $_.Cpu }
$mo = ($offCpu | Measure-Object -Minimum).Minimum
$mn = ($onCpu  | Measure-Object -Minimum).Minimum
function Median($xs) { $s = $xs | Sort-Object; $n = $s.Count; if ($n -eq 0) { return 0 }; if ($n % 2) { $s[[int](($n-1)/2)] } else { ($s[$n/2 - 1] + $s[$n/2]) / 2 } }
$medOff = Median $offCpu
$medOn  = Median $onCpu
$disjoint = (($offCpu | Measure-Object -Minimum).Minimum -gt ($onCpu | Measure-Object -Maximum).Maximum) -or
            (($onCpu | Measure-Object -Minimum).Minimum -gt ($offCpu | Measure-Object -Maximum).Maximum)
# ⚠Label the arms by what they ARE, not by what the first flag this harness measured happened to
# mean. It was written for an opt-OUT flag and printed "memo OFF/ON", which reads backwards for
# every opt-IN flag — and a backwards label on a disjoint result is how a win gets thrown away.
Write-Host ("CPU-s: {0} SET min={1}   UNSET min={2}   delta={3:P2} (negative = SET is faster)   disjoint={4}" -f `
  $Flag, $mn, $mo, (($mn - $mo) / $mo), $disjoint)
Write-Host ("CPU-s: {0} SET median={1}  UNSET median={2}  delta={3:P2}  (the median is the verdict when min and median disagree)" -f `
  $Flag, $medOn, $medOff, (($medOn - $medOff) / $medOff))
Write-Host ("order: SET seq={0}   UNSET seq={1}   (which run each measurement was, 1-based)" -f `
  (($on | ForEach-Object { $_.Seq }) -join '/'), (($off | ForEach-Object { $_.Seq }) -join '/'))
Write-Host ("wall-s: SET {0}   UNSET {1}   (kept for context; CPU is the verdict)" -f `
  (($on | ForEach-Object { $_.Wall }) -join '/'), (($off | ForEach-Object { $_.Wall }) -join '/'))

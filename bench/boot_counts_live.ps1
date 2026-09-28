# COUNTS FROM A BOOT THAT NEVER EXITS.
#
# ⭐**WHY COUNTS AND NOT TIME.** The boot's CPU floor on this box is ~5-8% and its SIGN FLIPS: a
# null test against a flag no code reads read -8.20% "disjoint" at n=2 and +6.99% at n=5, with
# adjacent identical runs 14% apart. Nothing worth 1-3% can ever be accepted or rejected there,
# and that is why roughly fifty levers in this tree are recorded as "in the noise" rather than
# decided.
#
# ⛔**AND THIS SCRIPT'S FIRST HEADER WAS WRONG ABOUT THE CURE.** It said "a COUNT does not care
# that the box is busy". That holds for a count of guest SEMANTICS — opcodes executed by a fixed
# workload, allocations of a deterministic program — because the program does the same work
# whatever else the box is doing. It does NOT hold for `exec`. How many invocations are entered
# as COMPILED depends on how far the background compiler got before execution reached each
# method, which is a RACE, and the race is decided by the same scheduler that makes the wall
# unmeasurable.
#
# Measured, same binary, no flags, two boots: `interpreted` = 11,936,203 then 11,371,032 — a
# **4.7% spread on the count itself**, which is the same order as the effects being looked for.
# ⇒ **Ask what a count DEPENDS on before treating it as noise-immune.** A count that depends on
# timing is a timing measurement wearing a count's clothes.
#
# The diag dump normally happens at main-return, and a server boot never returns — it is killed
# once `Done (` appears. So this arms the PERIODIC dumper and takes the LAST dump before the
# kill, which lands within `-EveryMs` of `Done`. Phase-aligned by EVENT, not by wall clock: a
# fixed time window is exactly how an earlier measurement ended up 86% server-tick and 14% boot.
#
#   bench\boot_counts_live.ps1 -Groups exec -Flags 'RAVA_WORKER_HOT=1','RAVA_JIT_GUEST_SUB=1'
param(
  [string]$Groups = 'exec',
  # ONE string, semicolon-separated. A [string[]] parameter loses everything after the first
  # comma when the call crosses a shell that does its own quoting, and the run then silently
  # measures a DIFFERENT configuration than the one asked for -- which is how the first use of
  # this script reported three flags and applied one.
  [string]$Flags = '',
  [string]$Tag = 'live',
  [int]$EveryMs = 2000,
  [int]$TimeoutS = 300
)
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$exe = Join-Path (Get-Location) 'target\release\rava.exe'
if (-not (Test-Path $exe)) { "missing $exe"; exit 3 }
& robocopy (Join-Path (Get-Location) 'world_pristine') (Join-Path (Get-Location) 'world') /MIR /NFL /NDL /NJH /NJS | Out-Null
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'; $env:RAVA_JIT_TIER0 = '1'; $env:RAVA_GSON_NATIVE = '1'; $env:RAVA_M3_BITSTORAGE = '1'
$env:RAVA_PROC_CAP_MB = '4096'; $env:RAVA_HARD_MB = '3584'
$env:RAVA_DIAG = $Groups; $env:RAVA_DIAG_EVERY_MS = "$EveryMs"
$applied = @()
foreach ($f in ($Flags -split ';' | Where-Object { $_.Trim() })) {
  $kv = $f.Trim().Split('=',2); Set-Item "env:$($kv[0])" $kv[1]; $applied += $kv[0]
}
$log = "bench\live_$Tag.log"
Remove-Item -LiteralPath $log, "$log.err" -EA SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $log -RedirectStandardError "$log.err" `
      -PassThru -WindowStyle Hidden
$null = $p.Handle
try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
$done = $false
while ($sw.Elapsed.TotalSeconds -lt $TimeoutS -and -not $p.HasExited) {
  Start-Sleep -Milliseconds 250
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'Done \(' -Quiet)) { $done = $true; break }
}
# Let one more periodic dump land AFTER Done, so the last block covers the whole boot.
Start-Sleep -Milliseconds ($EveryMs + 800)
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
foreach ($k in $applied) { Remove-Item "env:$k" -EA SilentlyContinue }
Remove-Item env:RAVA_DIAG, env:RAVA_DIAG_EVERY_MS -EA SilentlyContinue

$lines = @(Get-Content "$log.err" -EA SilentlyContinue)
$starts = @(0..($lines.Count-1) | Where-Object { $lines[$_] -match '^\[DIAG\] dump ' })
if (-not $starts.Count) { "no dump found in $log.err (reachedDone=$done)"; exit 4 }
$last = $starts[-1]
"reachedDone={0}  wall={1:n1}s (NOT a measurement)  dumps={2}  flags: {3}" -f $done, $sw.Elapsed.TotalSeconds, $starts.Count, ($applied -join ' ')
$lines[$last..($lines.Count-1)] | Where-Object { $_ -match '^\[DIAG\]' }

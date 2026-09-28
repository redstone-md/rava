# Worktree gate runner: THIS worktree's rava.exe against the shared checkout's
# COMPILED bench classes (bench/out and mc-cp.txt are untracked, so they exist
# only there). Read-only w.r.t. the shared checkout - logs land in this worktree.
#
# Prints the four correctness gates:
#   ChunkFill  expect 8790f04ace726ecf
#   RingScan   expect 37539c6c375d0c3  (N=8)   /  3ed21b4965 (N=4)
#   RingLane          expect the SAME hash as RingScan(N) - ARMS the lane (static lambda, MH kind 6)
#   RingLaneInstance  expect the SAME hash again - ARMS it through the INSTANCE lambda shape
#                     (MH kind 5/7) that the boot itself uses. ChunkFill and RingScan do NOT arm
#                     the lane at all: they replicate their loops inline and serially.
#
#   .\wt_gates.ps1 -Tag off
#   .\wt_gates.ps1 -Tag on -Envs @{RAVA_NO_VIRT_CLASS_MEMO='1'}
param([string]$Tag = 'run', [hashtable]$Envs = @{}, [int]$Ring = 8, [switch]$NoPower)
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe  = Join-Path $here '..\target\release\rava.exe'
$shared = 'D:\code\rava\bench'
$cp = (Join-Path $shared 'out') + ';' + ((Get-Content (Join-Path $shared 'mc-cp.txt') -Raw).Trim())

$base = @{ RAVA_JIT='1'; RAVA_PROC_CAP_MB='4096'; RAVA_HARD_MB='3584';
           RAVA_GSON_NATIVE='1'; RAVA_M3_BITSTORAGE='1'; RAVA_JIT_TIER0='1' }
# Disarm anything that would change WHAT is measured. RAVA_NO_ARGBUF_RECYCLE is an OPT-OUT:
# argument-buffer recycling is default-ON since 2026-07-29, so a stray opt-out inherited from
# a shell would silently gate a DIFFERENT engine than the one being shipped. The old opt-in
# name (RAVA_ARGBUF_RECYCLE) no longer exists and clearing it protected nothing.
foreach ($k in @('RAVA_NO_ARGBUF_RECYCLE','RAVA_NO_JIT_ITER_SHORTCUT','RAVA_NO_INSTANCEOF_MEMO',
                 'RAVA_VECGROW_STAT','RAVA_GC_STATS')) { Set-Item "env:$k" $null }
foreach ($k in $base.Keys) { Set-Item "env:$k" $base[$k] }
foreach ($k in $Envs.Keys) { Set-Item "env:$k" $Envs[$k] }

if (-not $NoPower) {
  $orig = (powercfg /getactivescheme); if ($orig -match '([0-9a-f-]{36})') { $orig = $Matches[1] }
  powercfg /setactive SCHEME_MIN | Out-Null
}

function Run-One($name, $argv, $tag) {
  $log = Join-Path $here "gate_$($tag).log"
  if (Test-Path $log) { Remove-Item $log -Force }
  $p = Start-Process -FilePath $exe -ArgumentList $argv -WorkingDirectory $here `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err" -PassThru
  try { $p.ProcessorAffinity = [IntPtr]0x0FFF } catch {}
  $p.WaitForExit(1800000) | Out-Null
  $h = (Select-String $log -Pattern 'hash|HASH|gate' | Select-Object -First 4)
  Write-Host "--- $name exit=$($p.ExitCode) ---"
  if ($h) { $h | ForEach-Object { Write-Host ("  " + $_.Line.Trim()) } }
  else { Get-Content $log -Tail 6 | ForEach-Object { Write-Host ("  " + $_) } }
}

Run-One 'ChunkFill' @('-cp', $cp, 'ChunkFill', '1', '64') "cf_$Tag"
Run-One "RingScan(N=$Ring)" @('-cp', $cp, 'RingScan', "$Ring") "rs_$Tag"
# THE PARALLEL GATE. ChunkFill and RingScan both replicate their loops INLINE and serially, so
# neither ever arms the gil-free ring lane — the lane could compute anything and both stay green.
# RingLane dispatches the same ring computation through CompletableFuture.supplyAsync, which is the
# shape the lane admits, and folds its hash in index order so only a WRONG VALUE can fail it.
# Expect the SAME hash as RingScan(N), with the lane on and off. It caught a real divergence on its
# very first run (RAVA_M3_FREEZE_RESOLVE + RAVA_GILFREE_ARRAY_OWNED together).
Run-One "RingLane(N=$Ring)" @('-cp', $cp, 'RingLane', "$Ring") "rl_$Tag"
# The boot's ring admits an INSTANCE lambda (MethodHandle kind 5/7, receiver in slot 0); RingLane's
# is a STATIC one (kind 6). Different marshalling shape - jit_direct_resolve's `shift` entry drops
# raw[0] for the instance form - so the static gate leaves the boot's own shape unproven. An audit
# caught exactly that gap. Same hash contract as the others.
Run-One "RingLaneInstance(N=$Ring)" @('-cp', $cp, 'RingLaneInstance', "$Ring") "rli_$Tag"

foreach ($k in $Envs.Keys) { Set-Item "env:$k" $null }
if (-not $NoPower) { powercfg /setactive $orig | Out-Null }

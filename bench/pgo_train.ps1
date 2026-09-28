# PGO TRAINING. Collects .profraw counters from a `-C profile-generate` build.
#
# It does NOT train on `net.minecraft.server.Main`, and that is forced, not a
# preference: LLVM writes the profile from an atexit handler, RAVA's server boot has
# no reachable clean exit (`stop` on stdin is accepted by nothing — verified by
# bench/stdin_stop_test.ps1: Done at 140 s, still alive 60 s after the write), and
# LLVM continuous mode (`%c`) leaves a profile llvm-profdata rejects as "truncated"
# on this Windows toolchain.
#
# So training runs the two canonical drivers over the SAME server.jar, both of which
# return from main and exit cleanly:
#   BootPhaseC  — Bootstrap.bootStrap through the datapack load = the serial prefix
#                 (blocks A+B+C), which is about two thirds of the boot wall
#   ChunkFill   — the worldgen ring, i.e. the Done window
# Between them they cover the boot's work; what is missing is only Main's own port
# bind and level-prep glue.
param(
  [string]$Exe = 'target\x86_64-pc-windows-msvc\release-fat\rava.exe',
  [switch]$SkipCompile
)

# NOT 'Stop': javac writes deprecation notes to stderr, which Windows PowerShell turns
# into a terminating NativeCommandError even on a clean compile. Exit code is checked.
$ErrorActionPreference = 'Continue'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"

if (-not $SkipCompile) {
  New-Item -ItemType Directory -Force out | Out-Null
  javac -nowarn -cp $mccp -d out bench\BootPhaseC.java bench\ChunkFill.java
  if ($LASTEXITCODE -ne 0) { throw "javac failed" }
  Write-Host ("compiled drivers: {0} class files" -f (Get-ChildItem out -Recurse -Filter *.class).Count)
}

Remove-Item Env:RAVA_JIT_THRESHOLD, Env:RAVA_NATIVE_LOADS, Env:RAVA_VERIFY_LOADS, `
            Env:RAVA_PROFILE_WALL, Env:RAVA_PROFILE_PC_STACKS, Env:RAVA_JIT_STATS, `
            Env:RAVA_COMPILE_TIME -ErrorAction SilentlyContinue
$env:RAVA_JIT = '1'
# MANDATORY for training: rava-cli's fast exit (main.rs, `std::process::exit` skipping
# Drop) leaves a 0-BYTE .profraw — the LLVM at-exit writer never completes. Measured
# by bench/pgo_write_probe2.ps1: same guest program, fast exit 0 B vs 5.24 MB with
# this set. It only changes teardown, so the counters are the same run.
$env:RAVA_NO_FAST_EXIT = '1'
Get-ChildItem target\pgo-data\*.profraw -ErrorAction SilentlyContinue | Remove-Item -Force

$runs = @(
  @{ tag = 'bootphasec'; args = @('BootPhaseC') },
  @{ tag = 'chunkfill';  args = @('ChunkFill', '1', '64') }
)
foreach ($r in $runs) {
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $env:LLVM_PROFILE_FILE = "D:\code\rava\target\pgo-data\$($r.tag)-%p.profraw"
  $out = "bench\pgo_$($r.tag).log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Exe -ArgumentList (@('-cp', $cp) + $r.args) `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  $p.WaitForExit()
  Write-Host ("{0}: exit={1} wall={2:N1}s" -f $r.tag, $p.ExitCode, $sw.Elapsed.TotalSeconds)
  Get-Content $out -Tail 2
}
Remove-Item Env:LLVM_PROFILE_FILE, Env:RAVA_NO_FAST_EXIT -ErrorAction SilentlyContinue

$raw = Get-ChildItem 'target\pgo-data\*.profraw' -ErrorAction SilentlyContinue
Write-Host ("profraw: {0} files, {1:N0} bytes" -f $raw.Count, (($raw | Measure-Object Length -Sum).Sum))

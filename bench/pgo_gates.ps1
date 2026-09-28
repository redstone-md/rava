# Correctness gates for a candidate binary. A build-level change cannot alter guest
# semantics in principle, but "in principle" is not a gate — these are the two hashes
# the tree keys on, plus the ring-lane gate that actually ARMS the lane.
#
# `nbtsave` is the FIFTH gate and the newest (ledger row 264). Every gate above it
# evaluates or scans; none of them SERIALISES, and the product regression that
# prompted it sat entirely in the phase after `Done` — the world save — where no
# instrument was looking. It found five defects on its first run, one of them a
# quadratic write buffer that exhausted the heap. Its hash is `java -Xint`'s.
param([string]$Exe = 'target\pgo-fat\rava.exe')
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$cp = "out;$mccp"
javac -nowarn -cp $mccp -d out bench\ChunkFill.java bench\RingScan.java bench\RingLane.java bench\RingLaneInstance.java bench\NbtSave.java
$env:RAVA_JIT = '1'
$stamp = if (Test-Path $Exe) { (Get-Item $Exe).LastWriteTime.ToString('yyyy-MM-dd HH:mm') } else { 'MISSING' }
"gating {0}  (built {1})" -f $Exe, $stamp

foreach ($g in @(
  @{ tag = 'chunkfill'; args = @('ChunkFill', '1', '64');  want = '8790f04ace726ecf' },
  @{ tag = 'ringscan';  args = @('RingScan');              want = '37539c6c375d0c3' },
  @{ tag = 'ringlane';  args = @('RingLane');              want = '' },
  @{ tag = 'ringlanei'; args = @('RingLaneInstance');      want = '' },
  @{ tag = 'nbtsave';   args = @('NbtSave', '64', '24');   want = 'ae44a34b42766bd4' }
)) {
  robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
  $out = "bench\gate_$($g.tag).log"
  Remove-Item $out, "$out.err" -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath $Exe -ArgumentList (@('-cp', $cp) + $g.args) `
        -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
        -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
  if (-not $p.WaitForExit(600000)) { $p.Kill(); Write-Host "$($g.tag): TIMEOUT"; continue }
  $hash = (Get-Content $out | Select-String 'HASH=([0-9a-f]+)' | Select-Object -First 1)
  $h = if ($hash) { $hash.Matches[0].Groups[1].Value } else { '<none>' }
  $verdict = if (-not $g.want) { 'record' } elseif ($h -eq $g.want) { 'PASS' } else { "FAIL (want $($g.want))" }
  Write-Host ("{0,-10} exit={1} hash={2} {3}" -f $g.tag, $p.ExitCode, $h, $verdict)
  Get-Content $out -Tail 1
}

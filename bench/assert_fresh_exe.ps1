# Refuse to measure a STALE binary.
#
# Dot-source this from a harness and call `Assert-FreshExe <path>` for every exe an arm will run.
#
# WHY THIS EXISTS. Measuring a stale binary is the single most expensive mistake available here,
# because it fails SILENTLY and produces a plausible number. It has happened three times in one
# session: a worktree measured against a candidate exe built before the day's defaults landed; a
# worktree runner picked up a stale bench/rava.toml with a different heap cap and read a 1.8x
# difference that was pure configuration; and boot_flag_ab.ps1's DEFAULT exe path
# (target\release\rava.exe) went 23 hours stale while every build went to target\probe, so an A/B
# ran a binary predating almost everything shipped that day and reported Done 112s against a real
# 65s. None of those announced themselves.
#
# The check is deliberately crude and cheap: if any tracked source file is newer than the exe, the
# exe cannot contain it. That direction never gives a false alarm on a genuinely fresh build.
#
# WHAT IT DOES NOT CATCH, so you know the boundary. It compares against the CURRENT tree's sources,
# so a binary built from an OLDER COMMIT but built RECENTLY passes — which is correct for the
# deliberate case (A/B-ing two vintages against each other, the way a round-over-round attribution
# run does) and useless for the accidental one. The failure it exists to stop is the opposite and
# far commoner shape: an exe that has simply not been rebuilt.

function Assert-FreshExe {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        # Warn and continue instead of stopping — for the rare case where you intend to run an
        # older build (bisecting, or A/B-ing two vintages on purpose).
        [switch]$WarnOnly
    )
    # Compare the exe against the sources of the tree IT WAS BUILT FROM, not the harness's tree.
    # An exe lives at <root>/target/<profile>/rava.exe, so its root is three levels up. Without
    # this, a cross-worktree A/B — a baseline exe from another checkout against the current one,
    # which is exactly how a tree-over-tree attribution runs — fails the check every time the main
    # tree is edited, and the "fix" a hurried operator reaches for is to disable the guard.
    $exeRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $Exe))
    $root = if ($exeRoot -and (Test-Path (Join-Path $exeRoot 'crates'))) {
        $exeRoot
    } else {
        Split-Path -Parent (Split-Path -Parent $PSCommandPath)
    }
    if (-not (Test-Path $Exe)) {
        Write-Host "STALE-EXE CHECK: '$Exe' does not exist. Build it first." -ForegroundColor Red
        if (-not $WarnOnly) { throw "missing exe: $Exe" }
        return
    }
    $exeTime = (Get-Item $Exe).LastWriteTime
    $newest = Get-ChildItem -Path (Join-Path $root 'crates') -Recurse -Include *.rs, Cargo.toml -File -EA SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $newest) { return }
    if ($newest.LastWriteTime -gt $exeTime) {
        $age = [int]($newest.LastWriteTime - $exeTime).TotalMinutes
        Write-Host ""
        Write-Host "STALE-EXE CHECK FAILED" -ForegroundColor Red
        Write-Host ("  exe    {0}   built {1}" -f $Exe, $exeTime) -ForegroundColor Red
        Write-Host ("  source {0}   saved {1}  ({2} min NEWER)" -f $newest.FullName, $newest.LastWriteTime, $age) -ForegroundColor Red
        Write-Host "  The binary cannot contain that source. Rebuild, or pass -WarnOnly if the vintage is deliberate." -ForegroundColor Red
        Write-Host ""
        if (-not $WarnOnly) { throw "stale exe: $Exe" }
    }
}

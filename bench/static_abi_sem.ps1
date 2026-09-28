param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [int]$NurseryKb = 0
)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$work = Join-Path $env:TEMP ('rava-static-abi-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$saved = @{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name] = $_.Value }
try {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    $env:RAVA_JIT = '1'
    $env:RAVA_JIT_TIER0 = '1'
    $env:RAVA_PROC_CAP_MB = '4096'
    $env:RAVA_HARD_MB = '3584'
    $env:RAVA_GSON_NATIVE = '1'
    $env:RAVA_M3_BITSTORAGE = '1'
    # Diagnostic admission only: require compiled callers and retain actual calls.
    $env:RAVA_BACKGROUND_JIT = '0'
    $env:RAVA_JIT_THRESHOLD = '16'
    $env:RAVA_INLINE_BUDGET = '0'
    $env:RAVA_DBG_REFABI = '1'
    if ($NurseryKb -gt 0) {
        $env:RAVA_NURSERY_KB = "$NurseryKb"
        $env:RAVA_GC_STATS = '1'
        $env:RAVA_GC_POISON = '1'
    }
    & javac -d $work "$PSScriptRoot\StaticAbiSem.java"
    if ($LASTEXITCODE -ne 0) { throw 'javac failed' }
    $reference = @(& java -Xint -cp $work StaticAbiSem)
    if ($LASTEXITCODE -ne 0) { throw 'Java reference failed' }
    $reference | Set-Content "$work\java.out"
    Get-FileHash -LiteralPath $Exe | Format-List | Out-File "$work\binary.txt"
    foreach ($mode in @('unset', 'prim', 'ref', 'both', 'fallback')) {
        Remove-Item Env:RAVA_PRIM_ABI, Env:RAVA_REF_ABI, Env:RAVA_NO_JIT_DIRECTMONO -ErrorAction SilentlyContinue
        if ($mode -in @('prim', 'both', 'fallback')) { $env:RAVA_PRIM_ABI = '1' }
        if ($mode -in @('ref', 'both', 'fallback')) { $env:RAVA_REF_ABI = '1' }
        if ($mode -eq 'fallback') { $env:RAVA_NO_JIT_DIRECTMONO = '1' }
        Get-ChildItem Env:RAVA* | Sort-Object Name | Format-Table -AutoSize | Out-File "$work\$mode.env"
        $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', ('"' + $work + '"'), 'StaticAbiSem') -WindowStyle Hidden -PassThru -RedirectStandardOutput "$work\$mode.out" -RedirectStandardError "$work\$mode.err"
        if (-not $p.WaitForExit(120000)) {
            $p.Kill()
            $p.WaitForExit()
            throw "$mode timed out (PID $($p.Id))"
        }
        "$mode exit=$($p.ExitCode)" | Tee-Object -FilePath "$work\results.txt" -Append
        if ($p.ExitCode -ne 0) { throw "$mode exited $($p.ExitCode)" }
        $actual = @(Get-Content "$work\$mode.out")
        if (($actual -join "`n") -cne ($reference -join "`n")) { throw "$mode differs from Java" }
        $diag = Get-Content "$work\$mode.err" -Raw
        if ($NurseryKb -gt 0 -and $diag -notmatch '\[GC-STATS\] minor=[1-9][0-9]*') {
            throw "$mode did not demonstrate a minor collection"
        }
        if ($mode -in @('prim', 'both', 'fallback')) {
            if ($diag -notmatch 'StaticAbiSem\.leaf\(IJFD\)D leaf=true verdict=0x') { throw "$mode did not resolve primitive leaf" }
            if ($diag -notmatch 'StaticAbiSem\.nonleaf\(IJFD\)D leaf=false verdict=REFUSED') { throw "$mode did not exercise rejected entry" }
        }
        if ($mode -in @('ref', 'both', 'fallback')) {
            if ($diag -notmatch 'StaticAbiSem\.reference\(Ljava/lang/Object;I\)Ljava/lang/Object; leaf=false verdict=0x') { throw "$mode did not resolve deep reference entry" }
        }
        "$mode PASS" | Tee-Object -FilePath "$work\results.txt" -Append
    }
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($name in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $name) -Value $saved[$name] }
    Write-Host "Evidence: $work"
}

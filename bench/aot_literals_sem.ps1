param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$root = Join-Path $env:TEMP ('rava-aot-literals-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory "$root/classes" -Force | Out-Null
$saved = @{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name] = $_.Value }
function Run-Owned([string]$program, [string[]]$arguments, [string]$label, [int]$expectedExit = 0) {
    $p = Start-Process -FilePath $program -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput "$root/$label.out" -RedirectStandardError "$root/$label.err"
    try {
        if (!$p.WaitForExit(60000)) { throw "$label timeout" }
        if ($p.ExitCode -ne $expectedExit) { throw "$label exit $($p.ExitCode)" }
    } finally {
        if (!$p.HasExited) { $p.Kill(); $p.WaitForExit() }
        $p.Dispose()
    }
}
try {
    Set-Content "$env:TEMP/rava-aot-literals-current.txt" $root
    Copy-Item -LiteralPath $Exe -Destination "$root/rava.exe"
    Get-FileHash "$root/rava.exe" | ConvertTo-Json | Set-Content "$root/binary.json"
    Run-Owned javac @('-d', "$root/classes", "$PSScriptRoot/AotLiteralSem.java") javac
    $arguments = @('-cp', "$root/classes", 'AotLiteralSem')
    Run-Owned java (@('-Xint') + $arguments) java
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    $env:RAVA_AOT_UNSAFE = '1'
    Run-Owned "$root/rava.exe" @('--prepare-aot', "$root/objects", "$root/classes") prepare
    $env:RAVA_JIT = '0'; $env:RAVA_AOT_PREPARED_DIR = "$root/objects"
    $env:RAVA_AOT_TRACE_RUN = '1'; $env:RAVA_NURSERY_KB = '64'; $env:RAVA_TRACE_ARENA = '1'
    Get-ChildItem Env:RAVA* | Select-Object Name,Value | ConvertTo-Json | Set-Content "$root/env.json"
    Run-Owned "$root/rava.exe" $arguments aot
    $expected = (Get-Content "$root/java.out" -Raw).Replace("`r", '').Trim()
    $actual = (Get-Content "$root/aot.out" -Raw).Replace("`r", '').Trim()
    if ($expected -ne $actual) { throw "Literal output mismatch: $root" }
    $trace = Get-Content "$root/aot.err" -Raw
    foreach ($method in @('literal', 'type', 'arrayType', 'keep')) {
        if (!$trace.Contains("[AOT-RUN] AotLiteralSem#$method")) { throw "Missing AOT entry $method" }
    }
    if (!$trace.Contains('[ARENA-GC]')) { throw 'No actual GC observed' }
    Remove-Item Env:RAVA_AOT_UNSAFE
    Run-Owned "$root/rava.exe" $arguments mismatch 1
    if (!(Get-Content "$root/mismatch.err" -Raw).Contains('configuration mismatch')) {
        throw 'Mismatched constant policy was not rejected explicitly'
    }
    Write-Output "PASS: literal/class identity, initialization, AOT entries, GC and configuration rejection. Evidence: $root"
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($name in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $name) -Value $saved[$name] }
}

param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$root = Join-Path $env:TEMP ('rava-aot-progress-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory "$root/classes" -Force | Out-Null
$saved = @{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name] = $_.Value }
function Run-Owned([string]$program, [string[]]$arguments, [string]$label) {
    $p = Start-Process -FilePath $program -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput "$root/$label.out" -RedirectStandardError "$root/$label.err"
    try {
        if (!$p.WaitForExit(30000)) { throw "$label timeout: $root" }
        if ($p.ExitCode -ne 0) { throw "$label exit $($p.ExitCode): $root" }
    } finally {
        if (!$p.HasExited) { $p.Kill(); $p.WaitForExit() }
        $p.Dispose()
    }
}
try {
    Set-Content "$env:TEMP/rava-aot-progress-current.txt" $root
    Copy-Item -LiteralPath $Exe -Destination "$root/rava.exe"
    Get-FileHash "$root/rava.exe" | ConvertTo-Json | Set-Content "$root/binary.json"
    Run-Owned javac @('-d', "$root/classes", "$PSScriptRoot/AotProgressSem.java") javac
    $arguments = @('-cp', "$root/classes", 'AotProgressSem')
    Run-Owned java (@('-Xint') + $arguments) java
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    $env:RAVA_JIT = '0'
    Run-Owned "$root/rava.exe" $arguments interpreter
    $env:RAVA_AOT_UNSAFE = '1'
    Run-Owned "$root/rava.exe" @('--prepare-aot', "$root/objects", "$root/classes") prepare
    $env:RAVA_AOT_PREPARED_DIR = "$root/objects"
    $env:RAVA_AOT_TRACE_RUN = '1'
    $env:RAVA_NURSERY_KB = '64'; $env:RAVA_TRACE_ARENA = '1'
    Get-ChildItem Env:RAVA* | Select-Object Name,Value | ConvertTo-Json | Set-Content "$root/env.json"
    Run-Owned "$root/rava.exe" $arguments aot
    foreach ($label in @('java', 'interpreter', 'aot')) {
        if ((Get-Content "$root/$label.out" -Raw).Trim() -ne '42') { throw "$label output mismatch" }
    }
    if (!(Get-Content "$root/aot.err" -Raw).Contains('[AOT-RUN] AotProgressSem#await')) {
        throw 'Prepared await entry missing'
    }
    if (!(Get-Content "$root/aot.err" -Raw).Contains('[ARENA-GC]')) { throw 'No actual GC observed' }
    Write-Output "PASS: asynchronous completion through prepared polling loop. Evidence: $root"
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($name in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $name) -Value $saved[$name] }
}

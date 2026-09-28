param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$root = Join-Path $env:TEMP ('rava-object-leaf-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory "$root/classes" -Force | Out-Null
$saved = @{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name] = $_.Value }
function Run-Owned([string]$program, [string[]]$arguments, [string]$label) {
    $p = Start-Process -FilePath $program -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput "$root/$label.out" -RedirectStandardError "$root/$label.err"
    try {
        if (!$p.WaitForExit(60000)) { throw "$label timeout" }
        if ($p.ExitCode -ne 0) { throw "$label exit $($p.ExitCode)" }
    } finally {
        if (!$p.HasExited) { $p.Kill(); $p.WaitForExit() }
        $p.Dispose()
    }
    return [IO.File]::ReadAllText("$root/$label.out")
}
try {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    Copy-Item -LiteralPath $Exe -Destination "$root/rava.exe"
    Get-FileHash "$root/rava.exe" | ConvertTo-Json | Set-Content "$root/binary.json"
    $null = Run-Owned 'javac' @('-d', ('"' + $root + '/classes"'), ('"' + $PSScriptRoot + '/ObjectLeafSem.java"')) 'javac'
    $null = Run-Owned "$root/rava.exe" @('--prepare-aot', ('"' + $root + '/objects"'), ('"' + $root + '/classes"')) 'prepare'
    $env:RAVA_JIT='0'
    $env:RAVA_AOT_PREPARED_DIR="$root/objects"; $env:RAVA_AOT_TRACE_RUN='1'
    $env:RAVA_XMX_MB='4096'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
    $cp = @('-cp', ('"' + $root + '/classes"'), 'ObjectLeafSem')
    foreach ($label in @('nonnull', 'faults')) {
        $arguments = $cp
        if ($label -eq 'nonnull') { $arguments += 'nonnull' }
        $expected = Run-Owned 'java' (@('-Xint') + $arguments) "java-$label"
        $actual = Run-Owned "$root/rava.exe" $arguments "aot-$label"
        if ($actual.Replace("`r", '').Trim() -ne $expected.Replace("`r", '').Trim()) { throw "$label output mismatch; evidence $root" }
    }
    $trace = [IO.File]::ReadAllText("$root/aot-nonnull.err")
    foreach ($method in @('call(', 'callNumbers(', 'callInstance(', 'callPrivate(')) {
        if (!$trace.Contains("[AOT-RUN] ObjectLeafSem#$method")) { throw "Missing prepared caller $method" }
    }
    foreach ($method in @('exchange(', 'numbers(', 'instanceExchange(', 'privateExchange(')) {
        if ($trace.Contains("[AOT-RUN] ObjectLeafSem#$method")) { throw "Leaf still uses AOT runner: $method" }
    }
    Write-Output "PASS: object results, aliasing, retention under allocation pressure, null effects and prepared caller entry. Evidence: $root"
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($name in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $name) -Value $saved[$name] }
}

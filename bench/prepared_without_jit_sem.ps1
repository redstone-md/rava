param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$root = Join-Path $env:TEMP ('rava-prepared-nojit-' + [guid]::NewGuid().ToString('N'))
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
    Set-Content (Join-Path $env:TEMP 'rava-prepared-nojit-current.txt') $root
    Copy-Item -LiteralPath $Exe -Destination "$root/rava.exe"
    Get-FileHash "$root/rava.exe" | ConvertTo-Json | Set-Content "$root/binary.json"
    $null = Run-Owned 'javac' @('-d', ('"' + $root + '/classes"'), ('"' + (Join-Path $PSScriptRoot 'CfgStackSem.java') + '"')) 'javac'
    $null = Run-Owned "$root/rava.exe" @('--prepare-aot', ('"' + $root + '/objects"'), ('"' + $root + '/classes"')) 'prepare'
    $env:RAVA_JIT='0'
    $env:RAVA_AOT_PREPARED_DIR="$root/objects"; $env:RAVA_AOT_TRACE_RUN='1'
    $env:RAVA_XMX_MB='4096'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
    Get-ChildItem Env:RAVA* | Sort-Object Name | Format-Table -AutoSize | Out-File "$root/execution.env"
    $arguments = @('-cp', ('"' + $root + '/classes"'), 'CfgStackSem')
    $expected = Run-Owned 'java' (@('-Xint') + $arguments) 'java'
    $actual = Run-Owned "$root/rava.exe" $arguments 'aot'
    if ($actual.Replace("`r", '').Trim() -ne $expected.Replace("`r", '').Trim()) { throw "output mismatch $root" }
    $trace = [IO.File]::ReadAllText("$root/aot.err")
    foreach ($method in @('bool', 'reference', 'wide', 'dereference')) {
        if (!$trace.Contains("[AOT-RUN] CfgStackSem#$method")) { throw "Missing prepared entry $method" }
    }
    Write-Output "PASS: Java boolean/ref/wide/switch/null results and prepared entries. Evidence: $root"
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($name in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $name) -Value $saved[$name] }
}



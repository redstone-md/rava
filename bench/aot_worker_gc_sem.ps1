param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference='Stop'
$Exe=(Resolve-Path -LiteralPath $Exe).Path
$root=Join-Path $env:TEMP ('rava-worker-gc-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory "$root/classes" -Force | Out-Null
$saved=@{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name]=$_.Value }
function Run-Owned([string]$program,[string[]]$arguments,[string]$label) {
    $p=Start-Process $program -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput "$root/$label.out" -RedirectStandardError "$root/$label.err"
    try {
        if (!$p.WaitForExit(60000)) { throw "$label timeout: $root" }
        if ($p.ExitCode -ne 0) { throw "$label exit $($p.ExitCode): $root" }
    } finally {
        if (!$p.HasExited) { $p.Kill(); $p.WaitForExit() }
        $p.Dispose()
    }
    [IO.File]::ReadAllText("$root/$label.out").Replace("`r",'').Trim()
}
try {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:'+$_.Name) }
    Copy-Item $Exe "$root/rava.exe"
    Get-FileHash "$root/rava.exe" | ConvertTo-Json | Set-Content "$root/binary.json"
    $null=Run-Owned javac @('-d', ('"'+$root+'/classes"'), ('"'+$PSScriptRoot+'/AotWorkerGcSem.java"')) javac
    $null=Run-Owned "$root/rava.exe" @('--prepare-aot', ('"'+$root+'/objects"'), ('"'+$root+'/classes"')) prepare
    $args=@('-cp', ('"'+$root+'/classes"'), 'AotWorkerGcSem')
    $expected=Run-Owned java (@('-Xint')+$args) java
    $env:RAVA_JIT='0'; $env:RAVA_AOT_PREPARED_DIR="$root/objects"
    $env:RAVA_AOT_TRACE_RUN='1'; $env:RAVA_NURSERY_KB='64'; $env:RAVA_TRACE_ARENA='1'
    $env:RAVA_XMX_MB='4096'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
    Get-ChildItem Env:RAVA* | Sort-Object Name | Format-Table -AutoSize | Out-File "$root/env.txt"
    $actual=Run-Owned "$root/rava.exe" $args rava
    if ($actual -ne $expected) { throw "Output mismatch: $root" }
    $trace=[IO.File]::ReadAllText("$root/rava.err")
    if (!$trace.Contains('[AOT-RUN] AotWorkerGcSem#work(')) { throw "Work did not run AOT: $root" }
    if (!$trace.Contains('[ARENA-GC]')) { throw "No collection: $root" }
    Write-Output "PASS: Java result, prepared work entry and GC. Evidence: $root"
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:'+$_.Name) }
    foreach ($name in $saved.Keys) { Set-Item -LiteralPath ('Env:'+$name) -Value $saved[$name] }
}

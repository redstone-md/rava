param([Parameter(Mandatory)][string]$Exe, [switch]$ExpectInline)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$root = Join-Path $env:TEMP ('rava-aot-call-cost-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory "$root/classes" -Force | Out-Null
$saved = @{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name] = $_.Value }
function Run-Owned([string]$program, [string[]]$arguments, [string]$label) {
    $process = Start-Process -FilePath $program -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput "$root/$label.out" -RedirectStandardError "$root/$label.err"
    try {
        $process.ProcessorAffinity = [IntPtr]0x0FFF
        if (!$process.WaitForExit(60000)) { throw "$label timeout" }
        if ($process.ExitCode -ne 0) { throw "$label exit $($process.ExitCode)" }
    } finally {
        if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
    return [IO.File]::ReadAllText("$root/$label.out")
}
try {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    Copy-Item -LiteralPath $Exe -Destination "$root/rava.exe"
    Get-FileHash "$root/rava.exe" | ConvertTo-Json | Set-Content "$root/binary.json"
    $null = Run-Owned 'javac' @('-d', ('"' + $root + '/classes"'), ('"' + $PSScriptRoot + '/AotCallCost.java"')) 'javac'
    $prep = Run-Owned "$root/rava.exe" @('--prepare-aot', ('"' + $root + '/objects"'), ('"' + $root + '/classes"')) 'prepare'
    if ($prep -notmatch 'compiled=4 refused=1') { throw "Unexpected coverage: $prep" }
    $env:RAVA_JIT='1'; $env:RAVA_JIT_THRESHOLD='2000000000'; $env:RAVA_BACKGROUND_JIT='0'
    $env:RAVA_AOT_PREPARED_DIR="$root/objects"
    $env:RAVA_XMX_MB='4096'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
    $cp = @('-cp', ('"' + $root + '/classes"'), 'AotCallCost')
    $env:RAVA_AOT_TRACE_RUN='1'
    $null = Run-Owned "$root/rava.exe" ($cp + @('10','12345')) 'entry'
    $entryMethods = @('calls', 'inline')
    if (!$ExpectInline) { $entryMethods += 'mix' }
    foreach ($method in $entryMethods) {
        if (![IO.File]::ReadAllText("$root/entry.err").Contains("[AOT-RUN] AotCallCost#$method")) { throw "Missing AOT entry: $method" }
    }
    if ($ExpectInline -and [IO.File]::ReadAllText("$root/entry.err").Contains('[AOT-RUN] AotCallCost#mix')) { throw 'Leaf still entered through AOT runner' }
    Remove-Item Env:RAVA_AOT_TRACE_RUN
    $rows = @()
    foreach ($seed in @('12345','-98765')) {
        foreach ($reverse in @($false,$true)) {
            $suffix = "$seed-$reverse"
            $arguments = $cp + @('2000000', $seed)
            if ($reverse) { $arguments += 'reverse' }
            $java = Run-Owned 'java' $arguments "java-$suffix"
            $actual = Run-Owned "$root/rava.exe" $arguments "aot-$suffix"
            $expected = [regex]::Match($java, '(?m)^RESULT=-?\d+').Value
            if (!$expected -or [regex]::Match($actual, '(?m)^RESULT=-?\d+').Value -ne $expected) { throw "Result mismatch: $suffix" }
            $rows += [pscustomobject]@{ seed=$seed; reverse=$reverse; result=$expected; call_ns=[long][regex]::Match($actual,'CALL_NS=(\d+)').Groups[1].Value; inline_ns=[long][regex]::Match($actual,'INLINE_NS=(\d+)').Groups[1].Value }
        }
    }
    $rows | ConvertTo-Json | Set-Content "$root/results.json"
    $rows | Format-Table
    Write-Output "PASS: matching Java results and confirmed prepared entry. Evidence: $root"
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($name in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $name) -Value $saved[$name] }
}

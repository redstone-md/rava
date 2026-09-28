param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$root = Join-Path $env:TEMP ('rava-prepared-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $root | Out-Null
Write-Output "Evidence: $root"
$saved = @{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name] = $_.Value }
function Run-Owned([string]$program, [string]$arguments, [string]$label, [int]$expectedExit = 0) {
    $out = Join-Path $root "$label.out"
    $err = Join-Path $root "$label.err"
    $process = Start-Process -FilePath $program -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
    try {
        if (!$process.WaitForExit(120000)) { throw "$label timeout" }
        $process.Refresh()
        if ($process.ExitCode -ne $expectedExit) { throw "$label exit $($process.ExitCode): $(Get-Content $err -Raw)" }
    } finally {
        if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
    return [IO.File]::ReadAllText($out).Replace("`r`n", "`n").Trim()
}
try {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    $env:RAVA_JIT='1'; $env:RAVA_JIT_THRESHOLD='2000000000'; $env:RAVA_BACKGROUND_JIT='0'
    $env:RAVA_AOT_TRACE_RUN='1'
    $env:RAVA_XMX_MB='4096'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
    $source = @'
public class PreparedSem {
    static { System.out.println("INIT"); }
    static int value(int n) {
        int x = 0;
        for (int i = 0; i < n; i++) x += i;
        return 100000 + x;
    }
    public static void main(String[] args) { System.out.println(value(10)); }
}
'@
    $classes = Join-Path $root 'classes'
    New-Item -ItemType Directory $classes | Out-Null
    Set-Content -LiteralPath "$classes/PreparedSem.java" -Value $source -Encoding ascii
    $null = Run-Owned 'javac' ('-d "{0}" "{0}/PreparedSem.java"' -f $classes) 'javac'
    $expected = Run-Owned 'java' ('-Xint -cp "{0}" PreparedSem' -f $classes) 'reference'
    $objects = Join-Path $root 'objects'
    $build = Run-Owned $Exe ('--prepare-aot "{0}" "{1}"' -f $objects,$classes) 'prepare'
    if ($build -notmatch '^PREPARED classes=1 compiled=[1-9][0-9]* refused=[0-9]+$') { throw "Unexpected preparation output: $build" }
    $before = Get-ChildItem $objects | Sort-Object Name | ForEach-Object { "$($_.Name) $($_.Length) $($_.LastWriteTimeUtc.Ticks) $((Get-FileHash $_.FullName).Hash)" }
    # Copying/re-timestamping the same executable must preserve artifact identity.
    $copy = Join-Path $root 'rava.exe'
    Copy-Item -LiteralPath $Exe -Destination $copy
    (Get-Item $copy).LastWriteTimeUtc = [datetime]::UtcNow.AddDays(-2)
    $env:RAVA_AOT_PREPARED_DIR = $objects
    foreach ($index in 1,2) {
        $actual = Run-Owned $copy ('-cp "{0}" PreparedSem' -f $classes) "run$index"
        if ($actual -ne $expected) { throw "run$index output mismatch: $actual / $expected" }
        if ((Get-Content "$root/run$index.err" -Raw) -notmatch '\[AOT-RUN\] PreparedSem#value\(I\)I') { throw 'single-call method did not use prepared code' }
    }
    $after = Get-ChildItem $objects | Sort-Object Name | ForEach-Object { "$($_.Name) $($_.Length) $($_.LastWriteTimeUtc.Ticks) $((Get-FileHash $_.FullName).Hash)" }
    if (Compare-Object $before $after) { throw 'runtime modified prepared artifacts' }
    $jar = Join-Path $root 'fixture.jar'
    $null = Run-Owned 'jar' ('--create --file "{0}" -C "{1}" PreparedSem.class' -f $jar,$classes) 'jar'
    $jarObjects = Join-Path $root 'jar-objects'
    $null = Run-Owned $Exe ('--prepare-aot "{0}" "{1}"' -f $jarObjects,$jar) 'prepare-jar'
    $env:RAVA_AOT_PREPARED_DIR = $jarObjects
    $actual = Run-Owned $copy ('-cp "{0}" PreparedSem' -f $jar) 'jar-run'
    if ($actual -ne $expected) { throw 'prepared JAR output mismatch' }
    if ((Get-Content "$root/jar-run.err" -Raw) -notmatch '\[AOT-RUN\] PreparedSem#value\(I\)I') { throw 'JAR method did not use prepared code' }
    $invalid = Join-Path $root 'invalid'
    New-Item -ItemType Directory $invalid | Out-Null
    $env:RAVA_AOT_PREPARED_DIR = $invalid
    $null = Run-Owned $copy ('-cp "{0}" PreparedSem' -f $classes) 'incomplete' 1
    if ((Get-Content "$root/incomplete.err" -Raw) -notmatch 'no completed-build marker') { throw 'missing marker was not rejected' }
    Set-Content "$invalid/complete" 'different-compiler'
    $null = Run-Owned $copy ('-cp "{0}" PreparedSem' -f $classes) 'incompatible' 1
    if ((Get-Content "$root/incompatible.err" -Raw) -notmatch 'compiler identity mismatch') { throw 'incompatible build was not rejected' }
    Write-Output "PASS: $build; first-call code executed; fresh initialization matched Java; artifacts unchanged"
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($key in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $key) -Value $saved[$key] }
}

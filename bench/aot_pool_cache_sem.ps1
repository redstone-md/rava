param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$root = Join-Path $env:TEMP ('rava-aot-pool-cache-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $root | Out-Null
Write-Output "Evidence: $root"
$saved = @{}
Get-ChildItem Env:RAVA* | ForEach-Object { $saved[$_.Name] = $_.Value }
try {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    $env:RAVA_JIT='1'; $env:RAVA_JIT_THRESHOLD='16'; $env:RAVA_BACKGROUND_JIT='0'
    $env:RAVA_AOT_CLIF='1'; $env:RAVA_AOT_TRACE_RUN='1'
    $env:RAVA_AOT_DIR=Join-Path $root 'cache'
    $env:RAVA_XMX_MB='4096'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
    Get-FileHash -LiteralPath $Exe | ConvertTo-Json | Set-Content "$root/binary.json"
    Get-ChildItem Env:RAVA* | Sort-Object Name | Select-Object Name,Value | ConvertTo-Json | Set-Content "$root/env.json"
    foreach ($version in @(1,2)) {
        $dir = Join-Path $root "v$version"
        New-Item -ItemType Directory $dir | Out-Null
        $literal = $version * 100000
        @"
public class AotPoolVersion {
    static int value() { return $literal; }
    public static void main(String[] args) {
        int sum = 0;
        for (int i = 0; i < 1000; i++) sum += value();
        System.out.println(sum);
    }
}
"@ | Set-Content "$dir/AotPoolVersion.java"
        & javac -d $dir "$dir/AotPoolVersion.java"
        if ($LASTEXITCODE -ne 0) { throw 'javac failed' }
        & javap -c -classpath $dir AotPoolVersion | Set-Content "$dir/bytecode.txt"
        if ($LASTEXITCODE -ne 0) { throw 'javap failed' }
        $reference = @(& java -Xint -cp $dir AotPoolVersion)
        if ($LASTEXITCODE -ne 0 -or $reference.Count -ne 1 -or $reference[0] -ne ($version * 100000000).ToString()) {
            throw 'Unexpected Java reference'
        }
        $reference | Set-Content "$dir/java.out"
    }
    $sequence = @(1,2,2,1)
    for ($i = 0; $i -lt $sequence.Count; $i++) {
        $version = $sequence[$i]
        $dir = Join-Path $root "v$version"
        $out = Join-Path $root "run$i.out"
        $err = Join-Path $root "run$i.err"
        $p = Start-Process -FilePath $Exe -ArgumentList @('-cp', ('"'+$dir+'"'), 'AotPoolVersion') -WorkingDirectory $root -RedirectStandardOutput $out -RedirectStandardError $err -WindowStyle Hidden -PassThru
        if (-not $p.WaitForExit(120000)) { $p.Kill(); $p.WaitForExit(); throw 'AOT probe timeout' }
        $p.WaitForExit()
        $actual = (Get-Content $out -Raw).Trim()
        $expected = (Get-Content "$dir/java.out" -Raw).Trim()
        if ($p.ExitCode -ne 0 -or $actual -ne $expected) { throw "Version $version run ${i}: exit=$($p.ExitCode), expected=$expected, actual=$actual" }
        if ((Get-Content $err -Raw) -notmatch '\[AOT-RUN\] AotPoolVersion#value\(\)I') { throw 'No executable AOT route evidence' }
        $files = @(Get-ChildItem -LiteralPath $env:RAVA_AOT_DIR -File | Sort-Object Name)
        if (-not $files.Count) { throw 'No persisted AOT objects' }
        $files | Select-Object Name,Length,LastWriteTimeUtc | ConvertTo-Json | Set-Content "$root/cache$i.json"
        Write-Output "Version $version run $i PASS: $actual; objects=$($files.Count)"
    }
    $first = Get-Content "$root/cache1.json" -Raw
    if ($first -ne (Get-Content "$root/cache2.json" -Raw) -or $first -ne (Get-Content "$root/cache3.json" -Raw)) {
        throw 'Returning to cached versions changed the persisted object set'
    }
} finally {
    Get-ChildItem Env:RAVA* | ForEach-Object { Remove-Item -LiteralPath ('Env:' + $_.Name) }
    foreach ($key in $saved.Keys) { Set-Item -LiteralPath ('Env:' + $key) -Value $saved[$key] }
}

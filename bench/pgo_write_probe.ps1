# Why did the training runs leave 0-byte .profraw? Isolates the launcher (Start-Process
# vs a direct child) from the workload (trivial exit vs a full guest run).
Set-Location $PSScriptRoot\..
Get-ChildItem target\pgo-data\*.profraw -ErrorAction SilentlyContinue | Remove-Item -Force
$exe = 'target\x86_64-pc-windows-msvc\release-fat\rava.exe'

$env:LLVM_PROFILE_FILE = 'D:\code\rava\target\pgo-data\direct-%p.profraw'
& $exe --help 2>&1 | Out-Null

$env:LLVM_PROFILE_FILE = 'D:\code\rava\target\pgo-data\startproc-%p.profraw'
$p = Start-Process -FilePath $exe -ArgumentList @('--help') -WorkingDirectory (Get-Location) `
      -RedirectStandardOutput 'bench\pgo_probe.log' -RedirectStandardError 'bench\pgo_probe.log.err' `
      -PassThru -WindowStyle Hidden
$p.WaitForExit()

Remove-Item Env:LLVM_PROFILE_FILE
Get-ChildItem target\pgo-data | Select-Object Name, Length

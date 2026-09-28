# Narrows the 0-byte .profraw: does ANY guest run break the writer, and does the
# CLI's fast-exit path (process::exit while VM threads are still live) cause it?
Set-Location $PSScriptRoot\..
javac -nowarn -d out bench\PgoHello.java
$mccp = (Get-Content 'bench\mc-cp.txt' -Raw).Trim()
$exe = 'target\x86_64-pc-windows-msvc\release-fat\rava.exe'
Get-ChildItem target\pgo-data\*.profraw -ErrorAction SilentlyContinue | Remove-Item -Force
$env:RAVA_JIT = '1'

foreach ($case in @(
  @{ tag = 'hello';        env = @{}; args = @('-cp', 'out', 'PgoHello') },
  @{ tag = 'hello-nofast'; env = @{ RAVA_NO_FAST_EXIT = '1' }; args = @('-cp', 'out', 'PgoHello') },
  @{ tag = 'hellocp';      env = @{}; args = @('-cp', "out;$mccp", 'PgoHello') }
)) {
  Remove-Item Env:RAVA_NO_FAST_EXIT -ErrorAction SilentlyContinue
  foreach ($k in $case.env.Keys) { Set-Item -Path "Env:$k" -Value $case.env[$k] }
  $env:LLVM_PROFILE_FILE = "D:\code\rava\target\pgo-data\$($case.tag)-%p.profraw"
  $p = Start-Process -FilePath $exe -ArgumentList $case.args -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput "bench\pgo_$($case.tag).log" -RedirectStandardError "bench\pgo_$($case.tag).log.err" `
        -PassThru -WindowStyle Hidden
  $p.WaitForExit()
  Write-Host ("{0}: exit={1} out={2}" -f $case.tag, $p.ExitCode, (Get-Content "bench\pgo_$($case.tag).log" -Tail 1))
}
Remove-Item Env:LLVM_PROFILE_FILE, Env:RAVA_NO_FAST_EXIT -ErrorAction SilentlyContinue
Get-ChildItem target\pgo-data | Select-Object Name, Length

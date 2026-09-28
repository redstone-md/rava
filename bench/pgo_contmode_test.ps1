# Does LLVM continuous profile mode (%c) work on this toolchain/OS? The server has
# no reachable clean-shutdown path (stdin `stop` never lands), so a killed process
# must still leave a valid .profraw or PGO training is impossible.
Set-Location $PSScriptRoot\..
Get-ChildItem target\pgo-data\*.profraw -ErrorAction SilentlyContinue | Remove-Item -Force
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:LLVM_PROFILE_FILE = 'D:\code\rava\target\pgo-data\c-%p%c.profraw'
$env:RAVA_JIT = '1'
$p = Start-Process -FilePath 'target\x86_64-pc-windows-msvc\release-fat\rava.exe' `
      -ArgumentList @('-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput 'bench\pgo_cont.log' `
      -RedirectStandardError 'bench\pgo_cont.log.err' -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 40
$p.Kill(); $p.WaitForExit()
Remove-Item Env:LLVM_PROFILE_FILE
Get-ChildItem target\pgo-data | Select-Object Name, Length

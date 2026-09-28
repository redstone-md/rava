# Does the server's console reader accept `stop` on stdin under RAVA? A clean guest
# exit is what PGO training needs (LLVM writes the profile from an atexit handler;
# continuous mode `%c` leaves a TRUNCATED profile on Windows — llvm-profdata refuses
# it). stdout/stderr are pumped asynchronously so a full pipe buffer cannot deadlock.
param([string]$Exe = 'target\release-fat\rava.exe', [int]$TimeoutSec = 200)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
$env:RAVA_JIT = '1'
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Resolve-Path $Exe).Path
# Windows PowerShell's ProcessStartInfo has no ArgumentList — build the string.
$psi.Arguments = "-cp `"$cp`" net.minecraft.server.Main --nogui"
$psi.WorkingDirectory = (Get-Location).Path
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true

$p = New-Object System.Diagnostics.Process
$p.StartInfo = $psi
$sb = New-Object System.Text.StringBuilder
$onOut = Register-ObjectEvent $p OutputDataReceived -Action { if ($EventArgs.Data) { [void]$Event.MessageData.AppendLine($EventArgs.Data) } } -MessageData $sb
$onErr = Register-ObjectEvent $p ErrorDataReceived -Action { }
[void]$p.Start(); $p.BeginOutputReadLine(); $p.BeginErrorReadLine()

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$reached = $false
while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
  Start-Sleep -Milliseconds 500
  if ($p.HasExited) { break }
  if ($sb.ToString() -match 'Done \(') { $reached = $true; break }
}
Write-Host ("Done reached={0} at {1:N1}s" -f $reached, $sw.Elapsed.TotalSeconds)
if ($reached) {
  $p.StandardInput.WriteLine('stop'); $p.StandardInput.Flush()
  $exited = $p.WaitForExit(60000)
  Write-Host ("clean exit after stop = {0} (total {1:N1}s)" -f $exited, $sw.Elapsed.TotalSeconds)
}
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
Unregister-Event -SourceIdentifier $onOut.Name; Unregister-Event -SourceIdentifier $onErr.Name
$sb.ToString() -split "`n" | Select-Object -Last 5

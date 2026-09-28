# WHAT DOES THE JVM'S 6 s ACTUALLY BUY, AND HOW MUCH OF IT IS FREE?
#
# `java -version` reports "mixed mode, SHARING" and opens classes.jsa at 0.008s, so the JVM's
# boot starts from a pre-parsed, pre-verified, pre-linked image of the core JDK. rava parses
# every class from scratch. This counts the split on the REAL boot: classes served from the
# shared archive vs read from a file, and the wall/Done beside it.
Set-Location $PSScriptRoot\..
$cp = 'out;' + ((Get-Content 'bench\mc-cp.txt' -Raw).Trim())
robocopy world_pristine world /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
$out = 'bench\jvm_cl.log'
Remove-Item -LiteralPath $out, "$out.err", 'bench\jvm_cl_classes.txt' -ErrorAction SilentlyContinue
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath 'java' -ArgumentList @(
      '-Xlog:class+load=info:file=bench\jvm_cl_classes.txt',
      '-cp', $cp, 'net.minecraft.server.Main', '--nogui') `
      -WorkingDirectory (Get-Location) -RedirectStandardOutput $out `
      -RedirectStandardError "$out.err" -PassThru -WindowStyle Hidden
$p.ProcessorAffinity = [IntPtr]0x0FFF
$dl = [DateTime]::Now.AddSeconds(180)
while ([DateTime]::Now -lt $dl) {
  Start-Sleep -Milliseconds 200
  if ($p.HasExited) { break }
  if (Test-Path $out) { if ((Get-Content $out -Tail 2) -match 'Done \(') { break } }
}
$sw.Stop()
$cpu = 0.0; try { $cpu = $p.TotalProcessorTime.TotalSeconds } catch {}
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
"wall={0:N2}s cpu={1:N1}s" -f $sw.Elapsed.TotalSeconds, $cpu
Get-Content $out | Select-String 'Done \(' | ForEach-Object { $_.Line }
if (Test-Path 'bench\jvm_cl_classes.txt') {
  $L = Get-Content 'bench\jvm_cl_classes.txt'
  "total class-load lines : {0}" -f $L.Count
  "from shared archive    : {0}" -f ($L | Select-String 'shared objects file' ).Count
  "from a jar/file        : {0}" -f ($L | Select-String 'source: (file|jar):' ).Count
  "generated/hidden       : {0}" -f ($L | Select-String '__JVM_DefineClass__|LambdaForm|Lambda\$' ).Count
}

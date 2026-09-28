# TLS CROSSING CENSUS - how many `LocalKey::with` crossings a real boot performs, per
# cluster, with phase attribution.
#
# WHY A COUNT AND NOT A WALL. This settles a question about two levers that claim the SAME
# removal (RAVA_TLS_CLUSTER's inlined cursor vs RAVA_DISPATCH_FASTFRAME's one-handle
# synthetic frame). Their deltas are substitutes over whatever they both remove, and a wall
# cannot separate substitutes on a contended box - a count can, and a count is immune to
# contention by construction. NO WALL IS REPORTED HERE.
#
# The run boots with the cluster cursor DISABLED, which is the arm that actually executes
# the crossings (see tls_cluster::stat) - the counter never touches the enabled fast path.
# `-Fast` additionally sets RAVA_DISPATCH_FASTFRAME=1; the DROP in the `frames` column
# between the two runs IS the population that lever removes, measured directly.
#
# Runs in its OWN universe/port/logs (junctions to out/libraries/versions), so it neither
# stomps nor is stomped by a sibling boot in D:\code\rava.
param(
  [Parameter(Mandatory=$true)][string]$Tag,
  [switch]$Fast,
  [ValidateSet('ecore','pcore')][string]$Cores = 'pcore',
  [string]$Exe = 'target\release\rava.exe',
  [int]$TimeoutS = 900
)
$ErrorActionPreference = 'Continue'
$mask = if($Cores -eq 'ecore'){ 0xF000 } else { 0x0FFF }
$run  = 'mcrun'
$out  = 'bench'
$cp   = 'out;' + ((Get-Content 'D:\code\rava\bench\mc-cp.txt' -Raw).Trim())

$env:RAVA_JIT='1'; $env:RAVA_PROC_CAP_MB='4096'; $env:RAVA_HARD_MB='3584'
$env:RAVA_GSON_NATIVE='1'; $env:RAVA_M3_BITSTORAGE='1'; $env:RAVA_JIT_TIER0='1'
$env:RAVA_TLS_STAT='1'; $env:RAVA_CALLBACK_STATS='1'
$env:RAVA_TLS_CLUSTER=$null
if($Fast){ $env:RAVA_DISPATCH_FASTFRAME='1' } else { $env:RAVA_DISPATCH_FASTFRAME=$null }

robocopy 'D:\code\rava\world_pristine' "$run\world" /MIR /NJH /NJS /NDL /NC /NP /R:1 /W:1 | Out-Null
$log="$out\tlscensus_$Tag.log"; $err="$out\tlscensus_$Tag.err"
Remove-Item $log,$err -EA SilentlyContinue

$sw=[Diagnostics.Stopwatch]::StartNew()
$p=Start-Process -FilePath $Exe -ArgumentList @('-cp',$cp,'net.minecraft.server.Main','--nogui') `
   -WorkingDirectory $run -RedirectStandardOutput $log -RedirectStandardError $err -PassThru
$p.ProcessorAffinity=[IntPtr]$mask

# Marker stopwatch: the [TLS-STAT] lines carry t=<ms since census armed>, which is within a
# few ms of process start, so these two clocks line up and the series becomes phase-split.
$marks=@{}
$want=@(
  @{k='ab';   pat='Environment:'},
  @{k='blockc';pat='Starting minecraft server version'},
  @{k='done'; pat='Done \('}
)
while($sw.Elapsed.TotalSeconds -lt $TimeoutS){
  if($p.HasExited){ break }
  if(Test-Path $log){
    foreach($w in $want){
      if(-not $marks.ContainsKey($w.k) -and (Select-String -Path $log -Pattern $w.pat -Quiet -EA SilentlyContinue)){
        $marks[$w.k]=[math]::Round($sw.Elapsed.TotalMilliseconds)
        Write-Host ("mark {0} at t={1}ms" -f $w.k,$marks[$w.k])
      }
    }
  }
  if($marks.ContainsKey('done')){ Start-Sleep -Milliseconds 1500; break }
  Start-Sleep -Milliseconds 250
}
if(-not $p.HasExited){ Stop-Process -Id $p.Id -Force -EA SilentlyContinue }
$env:RAVA_TLS_STAT=$null; $env:RAVA_CALLBACK_STATS=$null; $env:RAVA_DISPATCH_FASTFRAME=$null

Write-Host "--- markers ---"
$marks.GetEnumerator() | Sort-Object Value | ForEach-Object { Write-Host ("  {0} = {1}ms" -f $_.Key,$_.Value) }
Write-Host "--- last census lines ---"
Get-Content $err -EA SilentlyContinue | Select-String '\[TLS-STAT\]' | Select-Object -Last 3 | ForEach-Object { Write-Host $_.Line }
($marks.GetEnumerator() | ForEach-Object { "{0}={1}" -f $_.Key,$_.Value }) -join ' ' |
  Out-File "$out\tlscensus_$Tag.marks" -Encoding utf8

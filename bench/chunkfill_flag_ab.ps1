# Interleaved ChunkFill A/B for ONE env flag. Reads the numbers back from the run
# LOGS (canonical_run.ps1 reports via Write-Host, which `2>&1 | Out-String` does not
# capture — parsing the log is the reliable path).
#   .\chunkfill_flag_ab.ps1 -Flag RAVA_ARENA_STR_REFCELL -Seq off,on,on,off
# NOTE: ChunkFill is a PROXY. Its SETUP phase mirrors the real boot's dominant
# registry/datapack work; its FILL phase does NOT (the cached-world boot spends ~11ms
# in worldgen). Ship decisions still need the full boot to Done.
param(
  [Parameter(Mandatory=$true)][string]$Flag,
  [string]$Seq = 'off,on,on,off,off,on',
  [int]$StartIndex = 1
)
$root='D:\code\rava'; $here="$root\bench"
$csv="run,cfg,setup_ms,fill_ms,hash`n"; $i=$StartIndex-1
foreach($cfg in ($Seq -split ',')){
  $i++
  if($cfg -eq 'on'){ Set-Item "env:$Flag" '1' } else { Set-Item "env:$Flag" $null }
  $tag="fab_$($Flag)_$($i)_$cfg"
  & "$here\canonical_run.ps1" -Runs 1 -Tag $tag | Out-Null
  $log="$here\canon_$($tag)_1.log"
  $s=(Select-String $log -Pattern 'SETUP_MS=(\d+)' | Select-Object -First 1).Matches.Groups[1].Value
  $f=(Select-String $log -Pattern 'FILL_MS=(\d+)' | Select-Object -First 1).Matches.Groups[1].Value
  $h=(Select-String $log -Pattern 'ChunkFill_HASH=([0-9a-f]+)' | Select-Object -First 1).Matches.Groups[1].Value
  $csv += "$i,$cfg,$s,$f,$h`n"
  $csv | Out-File "$here\chunkfill_ab_$Flag.csv" -Encoding utf8
  Write-Host ("run{0} {1}: SETUP={2} FILL={3} hash={4}" -f $i,$cfg,$s,$f,$h)
}
Set-Item "env:$Flag" $null
"CF AB DONE" | Out-File "$here\chunkfill_ab_$Flag.done" -Encoding utf8

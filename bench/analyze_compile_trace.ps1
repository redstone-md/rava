param([Parameter(Mandatory)][string]$Directory)
$ErrorActionPreference = 'Stop'
$result = Get-Content -LiteralPath (Join-Path $Directory 'result.json') -Raw | ConvertFrom-Json
if (-not $result.success) { throw 'The boot did not reach Done' }
$startUs = [long]$result.start_epoch_ms * 1000
$doneUs = [long]$result.done_epoch_ms * 1000
$phases = @{}
$ended = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $Directory 'boot.err')) {
    if ($line -match '^\[COMPILE-PHASE\] bytecode=\d+ phase=(\S+) values=\d+ blocks=\d+ id=(\d+) elapsed_us=(\d+)$') {
        $id = [long]$Matches[2]
        if (-not $phases.ContainsKey($id)) { $phases[$id] = @{} }
        $phases[$id][$Matches[1]] = [long]$Matches[3]
    } elseif ($line -match '^\[COMPILE-END\] id=(\d+) bytecode=(\d+) start_epoch_us=(\d+) elapsed_us=(\d+) thread=ThreadId\((\d+)\)$') {
        $id = [long]$Matches[1]
        if ($ended.ContainsKey($id)) { throw "Duplicate compile ID $id" }
        $ended[$id] = [pscustomobject]@{
            id=$id; bytecode=[long]$Matches[2]; start_us=[long]$Matches[3]
            elapsed_us=[long]$Matches[4]; thread=[long]$Matches[5]
        }
    }
}
if (-not $ended.Count) { throw 'No completed compiler trace records' }
$rows = @($ended.Values | Where-Object {
    $_.start_us -ge $startUs -and ($_.start_us + $_.elapsed_us) -le $doneUs
} | Sort-Object start_us)
[long]$covered = 0
[long]$right = $startUs
foreach ($row in $rows) {
    $end = $row.start_us + $row.elapsed_us
    if ($end -gt $right) {
        $covered += $end - [Math]::Max($right, $row.start_us)
        $right = $end
    }
}
$unmatched = @($phases.Keys | Where-Object { -not $ended.ContainsKey($_) }).Count
$phaseTotals = @{}
foreach ($row in $rows) {
    if ($phases.ContainsKey($row.id)) {
        $p = $phases[$row.id]
        if ($p.ContainsKey('body.done') -and $p.ContainsKey('finalize.done') -and $p.ContainsKey('codegen.done')) {
            foreach ($entry in @{
                before_body_done=$p['body.done']
                finalize=($p['finalize.done'] - $p['body.done'])
                codegen=($p['codegen.done'] - $p['finalize.done'])
            }.GetEnumerator()) {
                if ($entry.Value -lt 0) { throw 'Non-monotonic phase timestamps' }
                $phaseTotals[$entry.Key] += $entry.Value
            }
        }
    }
}
$summary = [pscustomobject]@{
    completed_before_done=$rows.Count
    completed_outside_window=($ended.Count - $rows.Count)
    phase_ids_without_end=$unmatched
    compiler_interval_union_us=$covered
    driver_elapsed_sum_us=($rows | Measure-Object elapsed_us -Sum).Sum
    phase_elapsed_sums_us=$phaseTotals
    threads=@($rows | Group-Object thread | Select-Object Name,Count)
    longest=@($rows | Sort-Object elapsed_us -Descending | Select-Object -First 10)
    note='Elapsed intervals include waits and instrumentation. Sums are not CPU time. Only completed driver scopes are included; plans built before entry are excluded.'
}
$summary | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $Directory 'compile-times.json')
$summary | ConvertTo-Json -Depth 6

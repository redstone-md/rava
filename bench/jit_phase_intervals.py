"""Validate and summarize completed JIT phase intervals in a boot evidence directory."""
import argparse
import collections
import json
import pathlib
import re


def union_us(intervals):
    total = 0
    right = None
    for start, end in sorted(intervals):
        if right is None or start >= right:
            total += end - start
        elif end > right:
            total += end - right
        right = max(right if right is not None else end, end)
    return total


assert union_us([(1, 5), (2, 3), (4, 8), (8, 10)]) == 9
assert union_us([]) == 0
assert union_us([(1, 2), (4, 6)]) == 3
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("run_dir", type=pathlib.Path)
parser.add_argument("--output", type=pathlib.Path)
args = parser.parse_args()
run = args.run_dir
output = args.output or run
output.mkdir(parents=True, exist_ok=True)
result = json.loads((run / "result.json").read_text(encoding="utf-8-sig"))
assert result["success"] and result["instrumented"]
lo = result["start_epoch_ms"] * 1000
hi = result["done_epoch_ms"] * 1000
pattern = re.compile(r"^\[JIT-PHASE\] phase=(\S+) thread=(\S+) key=([0-9a-f]+) start_us=(\d+) elapsed_us=(\d+)$")
groups = collections.defaultdict(list)
threads = collections.defaultdict(list)
keys = collections.defaultdict(collections.Counter)
records = []
outside = 0
assert "[JIT-PHASE-ERROR]" not in (run / "boot.err").read_text(encoding="utf-8", errors="replace")
for line in (run / "phases.log").read_text(encoding="utf-8").splitlines():
    assert not line.startswith("[JIT-PHASE-DROPPED]"), line
    if not line.startswith("[JIT-PHASE]"):
        continue
    match = pattern.fullmatch(line)
    assert match, line
    phase, thread, key, start, elapsed = match.groups()
    start, elapsed = int(start), int(elapsed)
    end = start + elapsed
    a, b = max(lo, start), min(hi, end)
    if b < a or start >= hi or end <= lo:
        outside += 1
        continue
    groups[phase, thread].append((a, b))
    threads[thread].append((a, b))
    keys[phase, thread][key] += 1
    records.append(dict(phase=phase, thread=thread, key=key,
                        start_us=start, elapsed_us=elapsed, clipped_us=b-a))
assert records, "No in-window trace records"
rows = []
for (phase, thread), spans in sorted(groups.items()):
    counts = keys[phase, thread]
    rows.append(dict(phase=phase, thread=thread, completed_spans=len(spans),
                     unique_keys=len(counts), repeated_keys=sum(n > 1 for n in counts.values()),
                     inclusive_ms=round(sum(b-a for a,b in spans)/1000,3),
                     union_ms=round(union_us(spans)/1000,3)))
report = dict(run=str(run), start_us=lo, done_us=hi, wall_ms=result["wall_to_done_ms"],
              outside_spans=outside, phases=rows,
              all_phase_union_ms_by_thread={t:round(union_us(s)/1000,3) for t,s in threads.items()})
(output / "intervals.json").write_text(json.dumps(records), encoding="utf-8")
(output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
print(json.dumps(report, indent=2))

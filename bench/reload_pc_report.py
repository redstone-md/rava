"""Rank primary-thread PC samples inside certified ReloadBench phase boundaries.

Use ReloadBench --trace-phases with RAVA_PROFILE_PC=1 and RAVA_PC_TRACE=1.
This is primary-thread residency, not all-thread CPU cost or a speedup price.
"""
import argparse
from collections import Counter
import json
from pathlib import Path
import re

from pc_trace_report import short


class ReloadProfile:
    def __init__(self, output, trace, want="c956377e19071d7a"):
        self.output = output
        self.want = want
        self.samples = []
        for line in trace.splitlines():
            match = re.match(r"\[PC-TRACE\] (\d+) (.*)", line)
            if match:
                leaf = match[2].split(" |", 1)[0]
                self.samples.append((int(match[1]), short(leaf)))
        self.samples.sort()
        if not self.samples:
            raise ValueError("no primary-thread PC samples")

    def windows(self):
        marks = [(m[1], int(m[2])) for m in re.finditer(
            r"(?m)^RELOAD_PHASE name=([\w.]+) epoch_ms=(\d+)\r?$", self.output)]
        cold = re.search(r"RELOAD_MS=\d+.*? HASH=([0-9a-f]+)", self.output)
        warm = re.findall(r"(?m)^WARM_RESULT index=(\d+).*? HASH=([0-9a-f]+)\r?$", self.output)
        if not cold or cold[1] != self.want or not warm or any(int(i) != n or h != cold[1]
                                       for n, (i, h) in enumerate(warm)):
            raise ValueError("missing or inconsistent cold/warm certification")
        expected = ["registries.start", "cold.start", "cold.end"]
        for _ in warm:
            expected += ["warm.start", "warm.end", "warm.digest.end"]
        expected += ["cold.digest.end"]
        if [name for name, _ in marks] != expected:
            raise ValueError("missing, reordered or duplicate phase markers")
        if any(a[1] > b[1] for a, b in zip(marks, marks[1:])):
            raise ValueError("phase clock moved backwards")
        windows = [("registries", marks[0][1], marks[1][1]),
                   ("cold", marks[1][1], marks[2][1])]
        for n in range(len(warm)):
            start, end, digest = marks[3 + n * 3:6 + n * 3]
            windows += [(f"warm.{n}", start[1], end[1]),
                        (f"warm.digest.{n}", end[1], digest[1])]
        windows.append(("cold.digest", marks[-2][1], marks[-1][1]))
        return windows

    def report(self):
        reports = []
        for name, start, end in self.windows():
            counts = Counter(leaf for stamp, leaf in self.samples if start <= stamp < end)
            total = sum(counts.values())
            if not total:
                raise ValueError(f"no samples in {name}; trace is incomplete")
            reports.append({"phase": name, "wall_ms": end - start, "samples": total,
                            "symbols": [{"name": leaf, "samples": count,
                                         "percent": round(100 * count / total, 2)}
                                        for leaf, count in counts.most_common()]})
        return reports


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--want", default="c956377e19071d7a")
    args = parser.parse_args()
    profile = ReloadProfile(args.output.read_text(encoding="utf-8", errors="replace"),
                            args.trace.read_text(encoding="utf-8", errors="replace"), args.want)
    reports = profile.report()
    if args.json:
        args.json.write_text(json.dumps(reports, indent=2), encoding="utf-8")
    for report in reports:
        print(f"{report['phase']}: {report['wall_ms']} ms, {report['samples']} primary-thread samples")
        for row in report["symbols"][:12]:
            print(f"  {row['percent']:6.2f}% {row['samples']:6} {row['name']}")


if __name__ == "__main__":
    main()

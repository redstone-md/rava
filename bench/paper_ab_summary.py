#!/usr/bin/env python3
"""Summary of a bench/paper_ab.sh results directory: per build, the wall time from exec to the
server's "Done" line (mean, median, min, max, standard deviation) and mean user/sys CPU.

    bench/paper_ab_summary.py bench/results/<run>
"""
import csv
import statistics as st
import sys

rows = list(csv.DictReader(open(f"{sys.argv[1]}/runs.tsv"), delimiter="\t"))
for label in dict.fromkeys(r["label"] for r in rows):
    runs = [r for r in rows if r["label"] == label]
    wall = [float(r["wall_s"]) for r in runs if r["wall_s"] != "FAILED"]
    failed = len(runs) - len(wall)
    user = st.mean(float(r["user_s"]) for r in runs)
    sys_ = st.mean(float(r["sys_s"]) for r in runs)
    sd = st.stdev(wall) if len(wall) > 1 else 0.0
    print(f"{label:10} {runs[0]['commit'][:10]:10} n={len(wall)} failed={failed} wall mean {st.mean(wall):.2f} "
          f"median {st.median(wall):.2f} min {min(wall):.2f} max {max(wall):.2f} sd {sd:.2f}  cpu user {user:.1f} sys {sys_:.1f}")

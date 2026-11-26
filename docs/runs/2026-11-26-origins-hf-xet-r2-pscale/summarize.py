#!/usr/bin/env python3
"""Turn bench.sh's RESULT lines into a Markdown table.

    ./bench.sh | tee out.txt
    python3 summarize.py out.txt

Reports median and range per arm, because a single round on a shared link
says very little — the calibration run saw 1.7x between rounds of the
same arm.
"""
import re, statistics, sys
from collections import defaultdict

PAT = re.compile(
    r"RESULT\s+(\S+?)-(\d+)\s+rc=(\d+)\s+([\d.]+)\s*s\s+([\d.]+)\s*GB\s+([\d.]+)\s*GB/s(\s+INCOMPLETE)?"
)

def main(path):
    runs = defaultdict(list)
    for line in open(path):
        m = PAT.search(line)
        if not m:
            continue
        arm, _rnd, rc, secs, gb, rate, bad = m.groups()
        if rc != "0" or bad:
            runs[arm].append(None)
        else:
            runs[arm].append((float(secs), float(gb), float(rate)))

    print("| arm | runs | median | range | median time |")
    print("|---|---|---|---|---|")
    rows = []
    for arm, rs in runs.items():
        ok = [r for r in rs if r]
        if not ok:
            rows.append((0.0, f"| `{arm}` | {len(rs)} | — | all failed | — |"))
            continue
        rates = sorted(r[2] for r in ok)
        times = sorted(r[0] for r in ok)
        med = statistics.median(rates)
        rows.append((med,
            f"| `{arm}` | {len(ok)}/{len(rs)} | **{med:.3f} GB/s** | "
            f"{rates[0]:.3f}–{rates[-1]:.3f} | {statistics.median(times):.1f} s |"))
    for _, r in sorted(rows, reverse=True):
        print(r)

if __name__ == "__main__":
    main(sys.argv[1])

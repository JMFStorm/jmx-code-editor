#!/usr/bin/env python3
"""Print the stress results as a table, next to a baseline when given (and present)."""

import json
import os
import sys


def load(path):
    rows = {}
    if not path or not os.path.exists(path):
        return rows
    counts = {}
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        r = json.loads(line)
        if "perf" in r:
            key = f"{r['scenario']} · {r['perf']}"
            rows[key] = r
        else:
            base = f"{r['scenario']} · {r['timing']}"
            counts[base] = counts.get(base, 0) + 1
            rows[f"{base} #{counts[base]}"] = r
    return rows


def cell(r):
    if r is None:
        return ""
    if "perf" in r:
        return f"{r['ms']:9.1f} ms  rss {r['rss_kib'] / 1024:6.0f} MB  peak {r['peak_kib'] / 1024:6.0f} MB"
    return f"avg {r['avg_ms']:7.2f}  p95 {r['p95_ms']:7.2f}  max {r['max_ms']:7.2f} ms (n={r['n']})"


def main():
    now = load(sys.argv[1])
    base = load(sys.argv[2] if len(sys.argv) > 2 else None)
    if os.path.abspath(sys.argv[1]) == os.path.abspath(sys.argv[2] if len(sys.argv) > 2 else ""):
        base = {}
    width = max((len(k) for k in now), default=10)
    for k, r in now.items():
        b = base.get(k)
        line = f"{k:<{width}}  {cell(r)}"
        if b is not None:
            line += f"   | before: {cell(b)}"
        print(line)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Rects of the surface's section heads and chips in a `tern shot` layout JSON (CSS px)."""
import json, sys
d = json.load(open(sys.argv[1]))
for e in d["elements"]:
    p, r = e["path"], e["rect"]
    if not e["visible"] or r[3] <= 0 or r[1] + r[3] < 0 or r[1] > d["viewport"]["height"]:
        continue
    if p.endswith("div.sf-section-head"):
        print("head ", [round(x) for x in r])
    elif p.endswith("span.sf.sf-badge") or p.endswith("sf-title"):
        print("chip ", [round(x) for x in r])

"""Summarize matched arrival-decode timings and the contiguous zoom/pan path."""
import argparse
import json
import re
from collections import defaultdict
from pathlib import Path
from statistics import median, quantiles

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("navigation", type=Path)
parser.add_argument("viewport", type=Path)
parser.add_argument("output", type=Path)
parser.add_argument("--pan-ablation", type=Path, help="same build with automatic sibling enabled before and disabled after")
args = parser.parse_args()

def summarize(path, endpoint):
    rows = json.loads(path.read_text())
    cases = defaultdict(list)
    for r in rows:
        layout = "single" if "-single." in r["image"] else "multilayer"
        cases[layout, r["driver"], r["build"]].append(r)
    result = []
    for (layout, driver, build), data in cases.items():
        phases = dict.fromkeys(r["phase"] for r in data)
        medians = {p:round(median(r["ms"] for r in data if r["phase"] == p), 2) for p in phases}
        runs = [r for r in data if r["phase"] == endpoint]
        first = [r for r in data if r["phase"] == "firstFull"]
        summary = dict(layout=layout, driver=driver, build=build, runs=len(runs), phases_ms=medians,
                       sequence_ms=round(median(r["elapsed_from_preview_ms"] for r in runs), 2),
                       first_full_prepare_ms=round(median(float(r["stages"].get("prepare",0)) for r in first),2),
                       first_full_decode_ms=round(median(r["decode_ms"][0] for r in first),2),
                       first_full_gets=median(r["gets"] for r in first),first_full_bytes=median(r["bytes"] for r in first))
        if "east1" in phases:
            for direction in ("east", "south"):
                hops = [r["ms"] for r in data if r["phase"].startswith(direction)]
                summary[direction+"_hop_ms"] = round(median(hops), 2)
                summary[direction+"_hop_p90_ms"] = round(quantiles(hops,n=10,method="inclusive")[8],2)
        result.append(summary)
    return {"responses":len(rows),"groups":result}

overlap = defaultdict(list)
for section in args.navigation.with_suffix(".log").read_text().split("PROFILE ")[1:]:
    if not section.startswith("after ") or "-single." not in section.splitlines()[0]:
        continue
    matches = re.findall(r'message="arrival Tier-1"[^\n]*completed_before_read_end="(\d+)"',section)
    if matches:
        overlap["first_full"].append(int(matches[0]))
        if len(matches)>1:
            overlap["neighbor"].append(int(matches[1]))
report = dict(navigation=summarize(args.navigation,"sibling"),viewport=summarize(args.viewport,"backtrack"),
              completed_before_payload_read_return={k:median(v) for k,v in overlap.items()})
if args.pan_ablation:
    report["viewport_sibling_enabled_before_disabled_after"] = summarize(args.pan_ablation,"backtrack")
args.output.write_text(json.dumps(report,indent=2)+"\n")

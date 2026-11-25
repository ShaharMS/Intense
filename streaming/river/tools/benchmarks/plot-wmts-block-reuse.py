"""Plot matched driver-navigation measurements, using mean additive wall times."""
import argparse
import json
from pathlib import Path
from statistics import mean

import matplotlib.pyplot as plt
from matplotlib.patches import Patch

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("measurements", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
rows = json.loads(args.measurements.read_text())
cases = [
    ("preview", "preview", "Coarse preview"),
    ("preview", "firstFull", "Full after preview"),
    ("zoom", "parent", "Zoom parent"),
    ("zoom", "firstFull", "Child after parent"),
    ("zoom", "vertical", "Vertical neighbor"),
    ("zoom", "distant", "Distant tile"),
    ("zoom", "sibling", "Horizontal neighbor"),
]
colors = ["#167D78", "#467CA3", "#AE5577", "#A9B4BA"]
legend = ["Source preparation", "Decode¹", "Grok RGBA conversion", "Other elapsed time"]
fig, axes = plt.subplots(1, 2, figsize=(12, 6.5), sharex=True, sharey=True)
maximum = 0
for ax, driver in zip(axes, ["openjpeg", "jp2grok"]):
    for i, (flow, phase, label) in enumerate(cases):
        for j, build in enumerate(["before", "after"]):
            sample = [r for r in rows if r["driver"] == driver and "d6-single" in r["image"]
                      and (r["flow"],r["phase"],r["build"]) == (flow,phase,build)]
            measures = []
            for r in sample:
                prepare = float(r["stages"].get("prepare", 0))
                details = r["grok_ms"]
                codec = details.get("decompress", r["decode_ms"][0])
                convert = details.get("conversion", 0)
                remainder = r["ms"] - prepare - codec - convert
                if remainder < 0:
                    raise ValueError("Counters overlap on this request")
                measures.append((prepare, codec, convert, remainder))
            y = 3*i+j
            left = 0
            for color, durations in zip(colors, zip(*measures)):
                value = mean(durations)
                ax.barh(y, value, height=.82, left=left, color=color, alpha=.6 if j==0 else 1)
                left += value
            ax.text(left+1.1, y, f"{left:.0f}", va="center", fontsize=9)
            maximum = max(maximum, left)
    ax.set_title({"openjpeg":"OpenJPEG","jp2grok":"Native Grok"}[driver], loc="left", weight="bold")
    ax.set_yticks([3*i+.5 for i in range(len(cases))], [c[-1] for c in cases])
    ax.spines[["top","right","left"]].set_visible(False)
    ax.set_xlabel("Mean elapsed milliseconds")
    ax.grid(axis="x", alpha=.2)
    ax.set_axisbelow(True)
axes[0].set_xlim(0, maximum*1.1)
axes[0].invert_yaxis()
fig.suptitle("Original single-layer images   ·   Before (upper bar) and after (lower bar)", y=.97, weight="bold")
fig.legend([Patch(color=c) for c in colors],legend,loc="lower center",ncol=4,frameon=False,bbox_to_anchor=(.5,.075))
fig.text(.5,.04,"¹ Includes conversion when separate native counters were unavailable. Other includes encoding, waits and client overhead.",ha="center",fontsize=9)
fig.subplots_adjust(left=.16,right=.99,bottom=.23,top=.87,wspace=.09)
fig.savefig(args.output, dpi=160)

"""Plot the elapsed time of three consecutive WMTS requests, including background waits."""
import argparse
import json
from pathlib import Path
from statistics import mean

import matplotlib.pyplot as plt
from matplotlib.patches import Patch

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("measurements", type=Path)
parser.add_argument("output", type=Path)
parser.add_argument("--candidate", default="after")
parser.add_argument("--labels", nargs=2, default=["Synchronous pair", "Background sibling"])
parser.add_argument("--overlapped-tier1", action="store_true",
                    help="label source preparation as inclusive of concurrent Tier-1 work")
args = parser.parse_args()
rows = json.loads(args.measurements.read_text())
colors = ["#167D78", "#467CA3", "#AE5577", "#D09936", "#A9B4BA"]
phases = ("preview", "firstFull", "sibling")
fig, axes = plt.subplots(2, 2, figsize=(12, 5.6), sharex=True)
for axs, layout in zip(axes, ("single", "layers")):
    for ax, driver in zip(axs, ("openjpeg", "jp2grok")):
        for y, build in enumerate(("before", args.candidate)):
            left = 0
            for phase in phases:
                sample = [r for r in rows if layout in r["image"] and
                          (r["build"], r["driver"], r["phase"]) == (build, driver, phase)]
                durations = []
                for r in sample:
                    timing = r["stages"]
                    rendering = "render" in timing
                    decode = r["grok_ms"].get("decompress", r["decode_ms"][0]) if rendering else 0
                    conversion = r["grok_ms"].get("conversion", 0) if rendering else 0
                    parts = [float(timing.get("prepare", 0)), decode, conversion,
                             float(timing.get("shared_wait", 0))]
                    durations.append(parts + [max(0, r["ms"] - sum(parts))])
                for color, values in zip(colors, zip(*durations)):
                    value = mean(values)
                    ax.barh(y, value, left=left, height=.42, color=color)
                    left += value
                ax.plot(left, y, '|', color='#182C38', markersize=14)
                if phase != 'sibling':
                    ax.text(left, y-.27, f'{left:.0f}', ha='center', fontsize=8)
            ax.text(left+2, y, f'{left:.0f}', va='center', fontsize=9)
        ax.set_title(f'{"OpenJPEG" if driver=="openjpeg" else "Grok"} · {"single layer" if layout=="single" else "multiple layers"}',
                     loc='left', weight='bold')
        ax.set_yticks([0, 1], args.labels)
        ax.set_ylim(1.6, -.66)
        ax.spines[['top','right','left']].set_visible(False)
        ax.grid(axis='x', alpha=.18)
        ax.set_axisbelow(True)
axes[1][0].set_xlabel('Mean elapsed time (ms)')
axes[1][1].set_xlabel('Mean elapsed time (ms)')
fig.suptitle('Preview → first full tile → immediate neighbor', weight='bold', y=.98)
fig.text(.5,.92,'Ticks mark each HTTP response. A shared wait includes time spent by a background render.',ha='center')
fig.legend([Patch(color=c) for c in colors], [('Source + overlapped Tier-1' if args.overlapped_tier1 else 'Source preparation'),'Foreground decode','Grok RGBA conversion','Shared wait','Encoding and other elapsed'],
           loc='lower center',ncol=5,frameon=False)
fig.subplots_adjust(left=.15,right=.96,bottom=.15,top=.82,hspace=.64,wspace=.48)
fig.savefig(args.output,dpi=155)

"""Plot sequential, single-native-tile WMTS driver profiles on a common wall-time axis.

Pass a benchmark JSON and its matching log. Each PROFILE build trial image
marker introduces that scenario's requests in JSON order. Parallel tiles
require per-request trace IDs and cannot be plotted by this reader.
"""
import argparse
import json
import re
from collections import defaultdict
from pathlib import Path
from statistics import mean

import matplotlib.pyplot as plt
from matplotlib.ticker import MultipleLocator

COLORS = {
    "Source preparation": "#167D78",
    "Tier-1 entropy decode": "#CA7021",
    "Inverse wavelet": "#7959A1",
    "Other rendering": "#467CA3",
    "Other elapsed time": "#A9B4BA",
}


def read_codec_times(path):
    timings = defaultdict(list)
    for line in path.read_text().splitlines():
        if line.startswith("PROFILE "):
            _, build, trial, image = line.split()
            key = build, int(trial), image
        match = re.search(r"River codec tile=0 stage=(\w+) wall_ms=([0-9.]+)", line)
        if match:
            name, duration = match.groups()
            if name == "setup":
                timings[key].append({})
            timings[key][-1][name] = float(duration)
    return timings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("measurements", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    rows = json.loads(args.measurements.read_text())
    traces = read_codec_times(args.measurements.with_suffix(".log"))
    grouped = defaultdict(list)
    for row in rows:
        if len(row["hashes"]) != 1:
            raise ValueError("A sequential single-tile profile is required")
        key = row["build"], row["trial"], row["image"]
        codec = traces[key].pop(0)
        render = float(row["stages"]["render"])
        prepare = float(row["stages"]["prepare"])
        other_render = render - codec["tier1"] - codec["dwt"]
        values = (prepare, codec["tier1"], codec["dwt"], other_render, row["ms"] - prepare - render)
        if min(values) < 0:
            raise ValueError(f"Overlapping or unmatched timings in {key}")
        grouped[row["phase"], row["build"]].append(values)
    if any(traces.values()):
        raise ValueError("Unmatched codec events")
    fig, ax = plt.subplots(figsize=(9, 4.2), layout="constrained")
    builds = list(dict.fromkeys(row["build"] for row in rows))
    phases = list(dict.fromkeys(row["phase"] for row in rows))
    labels, positions = [], []
    for group, phase in enumerate(phases):
        for index, build in enumerate(builds):
            samples = grouped[phase, build]
            y = group * (len(builds) + 0.5) + index
            positions.append(y)
            phase_name = {"parent": "Parent", "firstFull": "Child"}.get(phase, phase)
            build_name = {"baseline": "Before", "mq": "Driver only", "both": "Driver + scheduler"}.get(build, build)
            labels.append(f"{phase_name}  ·  {build_name}")
            left = 0
            for (name, color), times in zip(COLORS.items(), zip(*samples)):
                duration = mean(times)
                ax.barh(y, duration, left=left, color=color, height=.67,
                        label=name if group == index == 0 else None)
                if duration >= 23:
                    ax.text(left + duration/2, y, f"{duration:.0f}", ha="center", va="center", color="white")
                left += duration
            ax.annotate(f"{left:.0f} ms", (left, y), xytext=(5, 0), textcoords="offset points", va="center")
    ax.set_yticks(positions, labels)
    ax.invert_yaxis()
    ax.set_xlabel("Mean elapsed milliseconds per request")
    ax.set_title("WMTS parent and first full-quality zoom child", loc="left", weight="bold")
    ax.xaxis.set_major_locator(MultipleLocator(50))
    ax.margins(x=.15)
    ax.spines[["top", "right", "left"]].set_visible(False)
    ax.grid(axis="x", alpha=.15)
    ax.set_axisbelow(True)
    ax.legend(loc="upper center", bbox_to_anchor=(.45, -.25), ncol=3, frameon=False)
    fig.savefig(args.output, dpi=160)


if __name__ == "__main__":
    main()

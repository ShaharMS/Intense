"""Check original WMTS preview/parent RGB hashes against an independent JPEG 2000 decoder.

Run after benchmark-wmts-ram.py --driver-navigation. Full-resolution tiles
are checked against the original reference PNGs by the navigation harness.
"""
import argparse
import hashlib
import json
from pathlib import Path

from PIL import Image

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("measurements", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
observations = json.loads(args.measurements.read_text())
checks = []
for name in sorted({row["image"] for row in observations}):
    for phase in ["parent", "preview"]:
        rows = [row for row in observations if row["image"] == name and row["phase"] == phase]
        if not rows:
            continue
        reduction = (1 if phase == "parent" else 3) if "d6-single" in name else 0
        layers = int(phase == "preview")
        offset_x = offset_y = 512 if phase == "parent" else 0
        if "requested_tiles" in rows[0]:
            specs = {tuple(row["requested_tiles"][0]) for row in rows}
            if len(specs) != 1:
                raise ValueError(f"ambiguous reference geometry for {name} {phase}")
            reduction, col, row, layers = specs.pop()
            offset_x, offset_y = 512 * col, 512 * row
        with Image.open(Path("corpus") / name) as image:
            image.reduce = reduction
            image.layers = layers
            if reduction == 0 and layers:
                # Pillow refreshes the JPEG 2000 tile arguments only when
                # reduce is nonzero. Explicitly select layer one at full size.
                tile = image.tile[0]
                decoder_args = (tile[3][0], reduction, layers, *tile[3][3:])
                image.tile = [tile._replace(args=decoder_args) if hasattr(tile, "_replace")
                              else (*tile[:3], decoder_args)]
            image.load()
            pixels = image.crop((offset_x, offset_y, offset_x + 512, offset_y + 512)).convert("RGB")
            reference = hashlib.sha256(pixels.tobytes()).hexdigest()
        observed = {row["hashes"][0] for row in rows}
        if observed != {reference}:
            raise ValueError(f"{name} {phase} differs from independent RGB reference")
        checks.append(dict(image=name, phase=phase, reduction=reduction, layers=layers,
                           sha256=reference, responses=len(rows)))
args.output.write_text(json.dumps(checks, indent=2) + "\n")

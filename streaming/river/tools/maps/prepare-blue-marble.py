#!/usr/bin/env python3
"""Download one NASA composite and prepare River's geographic base layer."""

import argparse
import json
from pathlib import Path
import shutil
from urllib.request import urlopen

from PIL import Image

NASA_JPEG = (
    "https://assets.science.nasa.gov/content/dam/science/esd/eo/images/bmng/"
    "bmng-base/july/world.200407.3x5400x2700.jpg"
)
HALF_EDGE = 4096


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path, nargs="?", default=Path("corpus"))
    maps = parser.parse_args().corpus / "maps"
    source = maps / "sources" / "blue-marble.jpg"
    source.parent.mkdir(parents=True, exist_ok=True)
    if not source.exists():
        temporary = source.with_suffix(".part")
        with urlopen(NASA_JPEG, timeout=30) as response, temporary.open("wb") as output:
            shutil.copyfileobj(response, output)
        temporary.replace(source)

    with Image.open(source) as image:
        if image.size != (5400, 2700):
            raise ValueError(f"Unexpected Blue Marble dimensions {image.size}")
        world = image.convert("RGB").resize((2 * HALF_EDGE, HALF_EDGE), Image.Resampling.BICUBIC)

    chunks = {}
    for x in range(2):
        path = maps / f"blue-marble-{x}.jp2"
        with path.with_suffix(".part").open("wb") as output:
            world.crop((x * HALF_EDGE, 0, (x + 1) * HALF_EDGE, HALF_EDGE)).save(
                output, format="JPEG2000", tile_size=(512, 512), num_resolutions=6,
                quality_mode="rates", quality_layers=[10], progression="RPCL", plt=True,
            )
        path.with_suffix(".part").replace(path)
        chunks[f"{x}/0"] = f"maps/{path.name}"

    path = maps / "catalog.json"
    catalog = json.loads(path.read_text()) if path.exists() else {"layers": {}}
    catalog["layers"]["blue-marble"] = {
        "title": "Blue Marble · July 2004",
        "attribution": "NASA Earth Observatory · Blue Marble Next Generation",
        "grid": "EPSG:4326",
        "bounds": [-180, -90, 180, 90],
        "minZoom": 0,
        "maxZoom": 4,
        "chunks": chunks,
    }
    temporary = path.with_suffix(".part")
    temporary.write_text(json.dumps(catalog, indent=2) + "\n")
    temporary.replace(path)


if __name__ == "__main__":
    main()

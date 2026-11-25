#!/usr/bin/env python3
"""Prepare a local River demo from the Survey of Israel's published 2015 imagery tiles."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from math import asinh, atan, degrees, floor, pi, radians, sinh, tan
from pathlib import Path
from time import sleep
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import urlopen
from uuid import uuid4

from PIL import Image

SERVICE = (
    "https://tiles.arcgis.com/tiles/JcXY3lLZni6BK4El/arcgis/rest/services/"
    + quote("תצלום_אוויר_2015") + "/MapServer/tile/"
)
BOUNDS = (34.269, 29.492, 35.853, 33.291)
ATTRIBUTION = "Survey of Israel · 2015 · Data.gov.il"


def coordinate(lon, lat, zoom):
    edge = 2**zoom
    return floor((lon + 180) / 360 * edge), floor((1 - asinh(tan(radians(lat))) / pi) / 2 * edge)


def tile_bounds(x, y, zoom):
    edge = 2**zoom
    latitude = lambda row: degrees(atan(sinh(pi * (1 - 2 * row / edge))))
    return [x / edge * 360 - 180, latitude(y + 1), (x + 1) / edge * 360 - 180, latitude(y)]


def download(cache, zoom, x, y):
    path = cache / f"{zoom}-{x}-{y}.tile"
    if not path.exists():
        for attempt in range(3):
            try:
                with urlopen(f"{SERVICE}{zoom}/{y}/{x}", timeout=15) as response:
                    data = response.read()
                break
            except HTTPError as error:
                if error.code == 404:
                    data = b""
                    break
                if error.code not in (429, 500, 502, 503, 504) or attempt == 2:
                    raise
                sleep(2 ** attempt)
            except (TimeoutError, URLError):
                if attempt == 2:
                    raise
                sleep(2 ** attempt)
        temporary = path.with_suffix(".part")
        temporary.write_bytes(data)
        temporary.replace(path)
    return path


def prepare_layer(corpus, destination, cache, pool, layer_id, title, root_zoom, max_zoom, rectangles):
    width = 1 << (max_zoom - root_zoom)
    groups = {}
    for xmin, ymin, xmax, ymax in rectangles:
        for y in range(ymin, ymax + 1):
            for x in range(xmin, xmax + 1):
                groups.setdefault((x // width, y // width), set()).add((x, y))

    chunks = {}
    for (root_x, root_y), tiles in sorted(groups.items()):
        paths = pool.map(lambda xy: download(cache, max_zoom, *xy), sorted(tiles))
        image = Image.new("RGBA", (width * 256, width * 256))
        populated = False
        for (x, y), path in zip(sorted(tiles), paths):
            if path.stat().st_size == 0:
                continue
            with Image.open(path) as tile:
                if tile.size != (256, 256):
                    raise ValueError(f"Unexpected dimensions for {path}")
                image.paste(tile.convert("RGBA"), ((x % width) * 256, (y % width) * 256))
            populated = True
        if not populated:
            continue
        output = destination / f"{layer_id}-{root_x}-{root_y}.jp2"
        image.save(output, tile_size=(512, 512), num_resolutions=6, quality_mode="rates",
                   quality_layers=[6], progression="RPCL", plt=True)
        chunks[f"{root_x}/{root_y}"] = output.relative_to(corpus).as_posix()
        print(f"{layer_id}: prepared {root_x}/{root_y}", flush=True)

    if not chunks:
        raise ValueError(f"No tiles downloaded for {layer_id}")
    bounds = [tile_bounds(*map(int, key.split("/")), root_zoom) for key in chunks]
    return {
        "title": title,
        "attribution": ATTRIBUTION,
        "grid": "EPSG:3857",
        "bounds": [min(b[0] for b in bounds), min(b[1] for b in bounds),
                   max(b[2] for b in bounds), max(b[3] for b in bounds)],
        "minZoom": root_zoom,
        "maxZoom": max_zoom,
        "chunks": chunks,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, default=Path("corpus"))
    args = parser.parse_args()
    maps = args.corpus / "maps"
    cache = maps / "sources" / "israel-2015-tiles"
    cache.mkdir(parents=True, exist_ok=True)
    destination = maps / f"israel-2015-demo-{uuid4().hex}"
    destination.mkdir()
    west, south, east, north = BOUNDS
    first = coordinate(west, north, 12)
    last = coordinate(east, south, 12)
    detail = []
    for lon, lat in [(34.7818, 32.0853), (35.2137, 31.7683)]:
        x, y = coordinate(lon, lat, 13)
        detail.append(((x - 1) * 8, (y - 1) * 8, (x + 2) * 8 - 1, (y + 2) * 8 - 1))
    catalog_path = maps / "catalog.json"
    catalog = json.loads(catalog_path.read_text()) if catalog_path.exists() else {"layers": {}}
    with ThreadPoolExecutor(max_workers=12) as pool:
        for layer_id, title, root, zoom, rectangles in [
            ("israel-2015-overview", "Israel · 2015 overview", 8, 12, [(*first, *last)]),
            ("israel-2015-detail", "Israel · 2015 · 2 m city samples", 13, 16, detail),
        ]:
            catalog["layers"][layer_id] = prepare_layer(
                args.corpus, destination, cache, pool, layer_id, title, root, zoom, rectangles)
    temporary = catalog_path.with_suffix(".part")
    temporary.write_text(json.dumps(catalog, indent=2) + "\n")
    temporary.replace(catalog_path)


if __name__ == "__main__":
    main()

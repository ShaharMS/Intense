#!/usr/bin/env python3
"""Publish georeferenced 8-bit RGB Survey of Israel rasters as River map chunks."""

import argparse
from concurrent.futures import ProcessPoolExecutor
from contextlib import ExitStack
import json
import math
import shutil
from pathlib import Path
from uuid import uuid4
from zipfile import ZipFile

import numpy as np
from PIL import Image
import rasterio
from rasterio.enums import ColorInterp, Resampling
from rasterio.transform import from_bounds
from rasterio.vrt import WarpedVRT
from rasterio.warp import transform_bounds

CRS = "EPSG:3857"
WORLD_EDGE = math.pi * 6378137
ROOT_ZOOM = 12
MAX_ZOOM = 16
EDGE = 256 << (MAX_ZOOM - ROOT_ZOOM)
SPAN = (2 * WORLD_EDGE) / (1 << ROOT_ZOOM)


def chunk_sources(sources):
    for source, source_crs in sources:
        if source_crs is None or source.count < 3 or source.dtypes[:3] != ("uint8",) * 3:
            raise ValueError(f"Expected a georeferenced 8-bit RGB raster: {source.name}")
        west, south, east, north = transform_bounds(source_crs, CRS, *source.bounds)
        if not all(map(math.isfinite, (west, south, east, north))):
            raise ValueError(f"Invalid map extent: {source.name}")
        left = max(0, math.floor((west + WORLD_EDGE) / SPAN))
        right = min(1 << ROOT_ZOOM, math.ceil((east + WORLD_EDGE) / SPAN))
        top = max(0, math.floor((WORLD_EDGE - north) / SPAN))
        bottom = min(1 << ROOT_ZOOM, math.ceil((WORLD_EDGE - south) / SPAN))
        for x in range(left, right):
            for y in range(top, bottom):
                yield (x, y), (source, source_crs)


def render_chunk(x, y, sources):
    west = x * SPAN - WORLD_EDGE
    north = WORLD_EDGE - y * SPAN
    transform = from_bounds(west, north - SPAN, west + SPAN, north, EDGE, EDGE)
    result = np.zeros((EDGE, EDGE, 4), dtype=np.uint8)
    for source, source_crs in sources:
        has_alpha = ColorInterp.alpha in source.colorinterp
        with WarpedVRT(source, src_crs=source_crs, crs=CRS, transform=transform, width=EDGE, height=EDGE,
                       add_alpha=not has_alpha, warp_mem_limit=32, resampling=Resampling.bilinear) as vrt:
            alpha = vrt.colorinterp.index(ColorInterp.alpha) + 1
            data = vrt.read([1, 2, 3, alpha]).transpose(1, 2, 0)
            valid = data[:, :, 3] != 0
            np.copyto(result, data, where=valid[:, :, None])
    return result


def extract_rasters(archive, root):
    destination = root / archive.stem
    if not destination.is_dir():
        staging = root / f".{archive.stem}-{uuid4().hex}"
        staging.mkdir(parents=True)
        try:
            with ZipFile(archive) as source:
                for member in source.infolist():
                    name = Path(member.filename).name
                    if Path(name).suffix.lower() not in {".tif", ".tiff", ".tfw", ".prj", ".aux", ".xml"}:
                        continue
                    with source.open(member) as contents, (staging / name).open("xb") as output:
                        shutil.copyfileobj(contents, output)
            if not any(path.suffix.lower() in {".tif", ".tiff"} for path in staging.iterdir()):
                raise ValueError(f"No GeoTIFFs in {archive}")
            staging.rename(destination)
        finally:
            if staging.exists():
                shutil.rmtree(staging)
    return sorted(path for path in destination.iterdir() if path.suffix.lower() in {".tif", ".tiff"})


def completed_chunk(path):
    if not path.is_file() or path.stat().st_size < 2:
        return False
    try:
        with path.open("rb") as stream:
            stream.seek(-2, 2)
            if stream.read() != b"\xff\xd9":
                return False
        with Image.open(path) as image:
            return image.format == "JPEG2000" and image.size == (EDGE, EDGE) and image.mode == "RGBA"
    except (OSError, SyntaxError, ValueError):
        return False


def prepare_chunk(job):
    (x, y), inputs, path = job
    with rasterio.Env(GDAL_CACHEMAX=32 * 1024**2, GDAL_NUM_THREADS="1"), ExitStack() as stack:
        sources = [(stack.enter_context(rasterio.open(name)), crs) for name, crs in inputs]
        image = render_chunk(x, y, sources)
    if not np.any(image[:, :, 3]):
        return None
    temporary = path.with_suffix(".part")
    Image.fromarray(image).save(
        temporary, format="JPEG2000", tile_size=(512, 512), num_resolutions=6,
        quality_mode="rates", quality_layers=[6], progression="RPCL", plt=True,
    )
    temporary.replace(path)
    print(f"Prepared 2 m chunk {x}/{y}", flush=True)
    return f"{x}/{y}", path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, default=Path("corpus"))
    parser.add_argument("--resume", type=Path, help="Directory of chunks from the same source batch")
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("rasters", nargs="+", type=Path, help="GeoTIFFs or Survey ZIPs")
    args = parser.parse_args()
    maps = args.corpus / "maps"
    # A fresh directory keeps the current catalog valid during a rebuild.
    destination = args.resume or maps / f"israel-2015-{uuid4().hex}"
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    if not destination.resolve().is_relative_to(maps.resolve()):
        parser.error("--resume must point inside the map corpus")
    destination.mkdir(parents=True, exist_ok=bool(args.resume))
    chunks = {}
    extracted = maps / "sources" / "israel-geotiffs"
    rasters = []
    for source in args.rasters:
        if source.suffix.lower() == ".zip":
            rasters.extend((path, "EPSG:2039") for path in extract_rasters(source, extracted))
        else:
            rasters.append((source, None))
    subsets = {}
    for path, fallback_crs in rasters:
        with rasterio.open(path) as source:
            if source.crs is None and fallback_crs:
                if source.transform.is_identity:
                    raise ValueError(f"Survey raster is missing its geotransform: {path}")
                print(f"Using {fallback_crs} for {path.name} from the Survey batch", flush=True)
            for tile, (_, crs) in chunk_sources([(source, source.crs or fallback_crs)]):
                subsets.setdefault(tile, []).append((path, str(crs)))

    jobs = []
    for (x, y), sources in sorted(subsets.items()):
        path = destination / f"{x}-{y}.jp2"
        if args.resume and completed_chunk(path):
            chunks[f"{x}/{y}"] = path.relative_to(args.corpus).as_posix()
        else:
            jobs.append(((x, y), sources, path))
    print(f"Reusing {len(chunks)} JP2 chunks; processing {len(jobs)} with {args.jobs} worker(s)", flush=True)
    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        for result in pool.map(prepare_chunk, jobs):
            if result:
                key, path = result
                chunks[key] = path.relative_to(args.corpus).as_posix()
    if not chunks:
        raise ValueError("No raster pixels intersect the Web Mercator tile grid")

    path = maps / "catalog.json"
    catalog = json.loads(path.read_text()) if path.exists() else {"layers": {}}
    coordinates = [tuple(map(int, key.split("/"))) for key in chunks]
    west = min(x for x, _ in coordinates) * SPAN - WORLD_EDGE
    east = (max(x for x, _ in coordinates) + 1) * SPAN - WORLD_EDGE
    south = WORLD_EDGE - (max(y for _, y in coordinates) + 1) * SPAN
    north = WORLD_EDGE - min(y for _, y in coordinates) * SPAN
    catalog["layers"]["israel-2015"] = {
        "title": "Survey of Israel · 2015",
        "attribution": "Survey of Israel · 2015 · Data.gov.il",
        "grid": CRS,
        "bounds": transform_bounds(CRS, "EPSG:4326", west, south, east, north),
        "minZoom": ROOT_ZOOM,
        "maxZoom": MAX_ZOOM,
        "chunks": chunks,
    }
    temporary = path.with_suffix(".part")
    temporary.write_text(json.dumps(catalog, indent=2) + "\n")
    temporary.replace(path)
    print(f"Published {len(chunks)} JP2 chunks to {path}")


if __name__ == "__main__":
    main()

# River map service

River Atlas at `/map` displays a fully lit globe with a Blue Marble base and an automatic
Survey of Israel overlay. Drag to move and scroll over the map to zoom. The viewer loads CesiumJS from jsDelivr. River serves configured
map tiles through its existing JPEG 2000 decoder and raster cache. City detail loads after you zoom in below 35 km and is released
when you zoom back out past 45 km. Map tile responses use `Cache-Control: no-store`.

Atlas lets Cesium submit imagery requests to the River origin without a per-server cap.
The browser and server negotiate HTTP/2 or HTTP/3 stream capacity. Applications that
call `/wmts` directly use their own client scheduling.

From the River directory, with Python and Pillow installed:

```sh
python tools/maps/prepare-blue-marble.py
cargo run --release -p river-server
```

Open `http://127.0.0.1:8080/map`. The preparation command downloads one [NASA Earth Observatory Blue Marble July 2004 composite](https://science.nasa.gov/earth/earth-observatory/blue-marble-next-generation/base-map/) and creates two tiled JP2s in `corpus/maps/`.

To assemble the local demo, run:

```sh
python tools/maps/prepare-israel-map-tiles.py
```

This downloads the [published Survey of Israel 2015 tiles](https://www.arcgis.com/home/item.html?id=1ef1f7b0909e468b93d7e224bd94c13b)
at zoom 12 across the available imagery extent and at zoom 16 for two roughly 12 km-wide sample areas
around Tel Aviv and Jerusalem. It assembles them into tiled JP2 chunks. River serves
the resulting overview and detail layers locally. Imagery is credited to the Survey
of Israel under the [government data portal license](https://data.gov.il/he/terms-of-use).

For nationwide full-resolution coverage, download the Survey's
[source archives](https://data.gov.il/he/datasets/israel_mapping_center/arb):

```sh
python tools/maps/download-israel-archives.py
```

If aria2c is installed, pass `--aria2c aria2c` to use its resumable download queue.

With Pillow, NumPy, and Rasterio installed, prepare the downloaded sheets:

```sh
python tools/maps/prepare-israel.py corpus/maps/sources/israel-archives/*.zip
```

The importer preserves georeferencing sidecars and uses Israel TM Grid (EPSG:2039)
for Survey ZIP rasters that have a geotransform but lack an embedded CRS. It creates
Web Mercator chunks at zooms 12–16. The base layer stays visible below their minimum
zoom and through uncovered areas. Restart River to load the updated catalog.

## Tile API

`GET /api/maps` returns map layer IDs, attribution, geographic bounds, grid and zoom
ranges. `GET /api/maps/{layer}/{z}/{x}/{y}` returns PNG map tiles (256 × 256 for
populated tiles). X counts east from the grid's west edge and Y counts south.
EPSG:4326 starts with two columns and one row. EPSG:3857 starts with one column and one
row. Missing coverage within the supported grid is transparent.

River reads `maps/catalog.json` under `RIVER_CORPUS_ROOT` at startup. Set
`RIVER_MAP_CATALOG` to select an explicit file. Each layer's `chunks` object maps
`x/y` coordinates at `minZoom` to corpus-relative JP2 keys. Each JP2 is aligned to one
complete chunk on that grid, with side length `256 × 2^(maxZoom − minZoom)`.

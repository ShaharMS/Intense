# River

## Goal

Explore browser-first streaming of selected JPEG 2000 raster regions with progressive quality, explicit capability limits, and complete request evidence.

## Approach

River sends WebP raster tiles over a viewport WebSocket session. It keeps source JPEG 2000 files immutable and uses SHA-256 identities, technical sidecars, and an object store for all derived artifacts. The browser renders decoded tile frames in WebGL and exposes its own decode/upload timings beside the server trace ID.

## Status

Active

## Current vertical slice

- Compilable Rust `axum` service with an OTLP tracing path, a technical manifest endpoint, and a versioned binary tile-frame protocol.
- WebSocket viewport requests with discrete zoom, pass ordering (`preview`, `gain`, `final`), and limited-capability control messages.
- A linked OpenJPEG decoder (`jpeg2k`/`openjpeg-sys`), executed in a blocking decode worker; it decodes a requested reference-grid region and converts the canonical 8-bit RGBA raster to WebP.
- A WebGL2 viewer with pointer-drag panning, cursor-anchored wheel zoom, and side controls intended for automation/precise requests.
- Local MinIO, Collector, Jaeger, Prometheus, Loki, and Grafana topology in `compose.yaml`.

The default asset is the immutable `Bretagne2.j2k` OpenJPEG corpus source, identified by SHA-256 `94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290`. Compose downloads it into a named volume and verifies that digest before River starts. The much larger PLISM tile-set sidecar remains registered as a future multi-source adapter target; it is not misrepresented as a decoded fast path.

## What River supports today

River is deliberately **JPEG 2000 input only**. The current registered asset is one local `.j2k`/`.jp2` codestream that OpenJPEG can convert to 8-bit grayscale, grayscale-plus-alpha, RGB, or RGBA. It supports:

- reference-grid viewport coordinates and power-of-two resolution reduction;
- region-of-interest decode through OpenJPEG (`decode_area`), without decoding the whole source for every tile;
- progressive `preview`, `gain`, and `final` raster frames over WebSocket;
- WebP as the *derived browser transport raster* and WebGL2 for display; WebP is not an accepted source format.

The server currently registers the bundled 2,592 × 1,944 `Bretagne2.j2k` source only. It does not yet ingest arbitrary paths at runtime, preserve georeferencing, expose JPIP/codestream transport, or make a multi-file mosaic selectable in the viewer.

## 30 GiB JPEG 2000 atlas in progress

`tools/download-sentinel-atlas.ps1` is a resumable downloader for a roughly 30 GiB test corpus made entirely of public Sentinel-2 Level-1C `.jp2` band files. Each acquisition combines the `B04`, `B03`, and `B02` JPEG 2000 components into RGB; four contiguous UTM 10S tiles form one east-west strip. Its generated `atlas.json` records every source URL and expected byte count.

Run it separately from Compose because the corpus is intentionally large:

```powershell
Set-Location streaming/river
./tools/download-sentinel-atlas.ps1 -TargetGiB 30
```

Compose mounts `corpus/cache/sentinel-atlas` read-only at `/app/sentinel-atlas`, but downloading it does **not** yet register it as a River asset. The pending JPEG2000-only source-layout resolver will map a virtual atlas viewport to the correct `.jp2` files and compose the three native bands into the streamed RGB raster. No conversion to a different input format is involved.

## Run

Docker is sufficient; no host Rust install is required.

```powershell
Set-Location streaming/river
docker compose up --build
```

Open [River viewer](http://127.0.0.1:8765), [River API](http://127.0.0.1:8080), [Jaeger](http://127.0.0.1:16686), [Grafana](http://127.0.0.1:3000), and [MinIO](http://127.0.0.1:9001). The viewer explicitly targets the local River API and WebSocket on port `8080`. Local credentials are intentionally development-only: `river` / `river-local-only-change-me`.

## Baselines

The corpus URLs and SHA-256 identities are tracked in [`corpus/manifest.json`](corpus/manifest.json). Compose obtains and verifies the default source automatically; the optional host baselines use `corpus/cache/`.

```powershell
./tools/measure-openjpeg-baseline.ps1
node ./tools/measure-protocol-fixture.mjs
```

The first command measures a single-threaded full-image FFmpeg/OpenJPEG decode. The second drives River's linked OpenJPEG decoder through the WebSocket and measures time to the first preview tile; it excludes browser decode/WebGL upload and object-store reads.

## Success criteria

- At 1920×1080 CSS pixels, ten loopback browser viewers achieve full preview coverage at p95 under 250 ms for fast-path assets.
- Every public corpus asset either meets the fast path or has a concrete, recorded limited-capability reason.
- Every viewport request has retained trace/ledger evidence plus a JSON and Markdown benchmark result.

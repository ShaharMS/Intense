# River

The shared streamer/tile-info contract for all channels is documented in [STREAMER-COMPATIBILITY.md](deploy/portable/agent-handoff/STREAMER-COMPATIBILITY.md). Deployed support follows `deploy/releases.json`; source edits alone do not update existing containers.


Verified release channels (`river:stable`, `river:lts`) and their current checkpoints, bundles and benchmark evidence are recorded in [`deploy/releases.json`](deploy/releases.json). Older checkpoint status statements below are historical.

Local Docker work uses the existing **river** Compose project with six reusable
backend/test slots: `river-stable`, `river-lts`, and `replica-1` through `replica-4`.
No per-experiment containers are permitted; stopped containers count toward six.
See [local Compose operations](deploy/LOCAL-COMPOSE.md). The viewer at
http://127.0.0.1:18765 defaults to replica-1 (direct API port18080).
Generate with `python tools/river-compose.py generate`, start with
`python tools/river-compose.py up`, and switch viewer routing at runtime with
`python tools/river-compose.py target replica-2 --runtime`.
Historical instructions below that create separate experiment containers/projects
are superseded by the fixed-slot workflow.

OpenJPEG uses bounded worker budgets per request. Automatic maxima are sixteen
workers for an isolated uncached render on machines with at least 16 available CPUs, four on 8-15, two on 4-7,
and one on smaller machines. Ordinary requests below 524,288 useful output pixels retain
a maximum of four. Compact full-image previews can also use extra workers when
many independent native-tile groups dominate the work. Larger requests share workers according to CPU capacity and
the number of active requests that can occupy decode slots; queued requests do
not reserve workers for slots they cannot yet use. Small-work/native-resolution guards still select one. The
shared extra-worker pool reserves CPU capacity for admitted jobs and prevents
worker oversubscription. Explicit numeric maxima and compact/throughput profiles
keep their existing upper-bound semantics. Unsigned 8-bit RGB conversion directly
interleaves decoded planes into RGBA; other formats keep the established converter.
Codec initialization stays synchronous by default (`OPJ_NUM_THREADS=0`); River
then applies each job's explicit budget. This avoids creating a disposable worker
pool for every small native tile when a large per-request maximum is configured.
An explicit `OPJ_NUM_THREADS` override is preserved.
`RIVER_PROFILE_SOURCE` additionally reports codec, conversion and preparation CPU
timings; container CPU counters require isolated diagnostic requests.
For internal OpenJPEG stage timings, set `RIVER_PROFILE_CODEC=1` and include
`jpeg2k=info` in `RUST_LOG` (for example, `river_server=info,jpeg2k=info`). This
reports per-tile wall time for setup, Tier-2 packet parsing, Tier-1 codeblock
decoding, window allocation, inverse DWT, MCT and DC shift through the normal
bounded console logger. It is off by default. Stage wall times include worker
waits and are not CPU times. Use isolated requests, and keep these diagnostic
runs separate from release timing evidence. The controlled cold benchmark's
`--capture-stage-logs` option saves logs from each fresh process after its timed
request; it requires `RIVER_PROFILE_SOURCE=1` on both comparison containers.

With source-read tracing enabled, successful SDK logical range records can include
`admissionMs`, `headersMs` and `bodyMs`. Admission ends when the source permit is
obtained. Logical header time includes SDK retries and backoff; each separate
`wireAttempt` records its own response-header wait. Unknown phases are omitted,
including native-transfer boundaries that are not measured. These intervals can
overlap across requests and must not be summed as server wall time. Trace context
is shared across asynchronous polls; tracing remains disabled by default.

The working-tree prototype also has an opt-in Linux serial S3 transport:
`RIVER_S3_SERIAL_TRANSPORT=curl` (a `grok` feature build with the native bridge).
`sdk` remains the default; batched payload requests keep the SDK transport in
both modes. The native path bounds handles and buffers, pins ETags/versions,
checks range responses, verifies TLS, and polls cancellation/deadlines. The read
policy API/UI exposes `serialTransport`; it persists with full configuration
export/import and takes precedence over the environment fallback when explicitly
saved. Requests snapshot the choice, including already registered readers.
Transient native failures allow at most three attempts inside one ten-second
range deadline, honor `Retry-After`, and use cancellable backoff. Range, version,
authentication and TLS failures are not retried. API measurements distinguish
native on-wire serial requests (including retries) from SDK logical serial reads;
initial probes and batched payloads are outside those counters. Proxy settings
remain unsupported by this prototype. Concurrent/progressive qualification is
still pending; it is not promoted merely from transport replay results.
Forced native transport currently has a measured tradeoff: isolated unindexed
loads improve, while some eight-client batches regress. Keep SDK for general
serving until contention-aware selection and progressive qualification pass.
The experimental `auto` choice waits for sustained serial misses on a fast,
unindexed grid and falls back to SDK when preparation is contended. It avoids
native connections for the small/single-tile cases. SDK remains the startup
default; automatic and forced-native choices can be persisted through the API.

Supported local JP2 files of at least32MiB retain their version-pinned file
range reader and use the same demand-driven technical views as object storage.
This avoids decoding a giant original tile or staging a full source copy for a
small request. File reads are reported as `file-range-hit`/`file-range-miss` and
file-specific routes; source-byte/range counters describe subset preparation.
Unsupported local layouts retain original-file decoding. Originals are never
rewritten, and the local filesystem provider enforces the captured ETag.

Complete-image lossless resolution prefixes can use a reduced technical header
instead of retaining unused high-resolution packet structures. This requires one
quality layer, compatible resolution-prefix progression, zero origins, and native
tile dimensions divisible by the reduction scale. PLT-indexed and supported inline
prefixes preserve their original entropy bytes. GPU complete-grid and incompatible
layouts retain the prior paths. For reduced views containing at least128 small
native tiles, OpenJPEG may decode independent tiles synchronously across the shared
worker budget and stitch their pixels before encoding. This avoids a worker barrier
inside every tiny tile. Original files and advertised image geometry are unchanged.

Multipart resolution-prefix reads can stop after all requested resolutions and
quality layers are covered, using chained TLM offsets and per-part PLT lengths.
For multilayer RLCP/RPCL sources without usable PLT, classic non-HT codeblock
styles are supported through stateful inline packet-header parsing, including
explicit precinct dimensions. SOP/EPH and other inline coding styles retain the
existing fallback. Part boundaries are validated rather than assumed to match
resolutions or layers. Original files are unchanged. The combo image enables
`RIVER_S3_MULTIPART_PLT=true` and `RIVER_S3_MULTIPART_PREFIX=true` by default;
explicit deployment overrides still take precedence. Fragmented layouts can
still need many GETs, and intermediate views may still reach request budgets.

The optional `RIVER_S3_SINGLE_PREFIX=true` cold-preview path streams PLT lengths
without allocating the full packet index, then constructs a temporary coarse
codestream from the original packet entropy. It currently requires a zero-origin,
single-native-tile, one-layer Part1 source with compatible resolution-prefix
progression. It handles the coarsest resolution and bounded intermediate previews
whose complete packet table exceeds the ordinary packet-array limit. The requested
full-image level must be at most4million pixels. Reversible and irreversible
wavelets are supported. Unknown-length multipart chains continue through normal
multipart discovery. Unsupported layouts retain existing paths; original files
are unchanged. This does not solve fragmented multi-native-tile preview or deep
tile GET counts. Candidate measurements and pixel checks are under
`reports/single-native-wide-m244/`; stable/LTS remains unchanged.

`RIVER_S3_STREAMED_PLT_ROI=true` enables bounded spatial selection for larger
single-native-tile PLT tables: one quality layer, reversible Part1, zero origin,
RPCL progression, and between one and eight million packets. It requires the
precinct ROI option. The server streams packet lengths into selected byte spans
and preserves a PLT index in its temporary codestream. It does not allocate a
complete packet descriptor array or change the source. Unsupported layouts keep
their existing serving path. Combo images enable this option; an explicit false
override disables it.

The local OpenJPEG dependency defers tag-tree, segment, and codeblock storage for
empty precincts while preserving initialized precinct geometry for ROI decisions.
This reduces decoder allocation pressure on sparse packet subsets. Provenance is
in `crates/river-openjpeg-sys/RIVER-PATCH.md`; release timings require the fixed
three-size server benchmark, independently of decoder-only diagnostics.

`RIVER_OPENHT_COARSE=true` additionally enables the `openhtj2k` worker for
eligible reversible zero-DWT coarse streams with multi-codeblock precincts,
reversible RGB transforms, and component quantization overrides. It preserves
original packet entropy and supports unsigned samples up to 16 bits within the
worker's 4-million-pixel and 66-MiB input limits. Small regions retain the region
decoder; explicit `RIVER_OPENHT_MIN_PACKETS` settings override automatic
selection. Existing packed/direct paths stay preferred. Unsupported layouts
retain the existing decoder. Checkpoint 253 measurements, pixel checks, and
eight-request capacity comparisons against 246 are under
`reports/coarse-worker-m249/`. This candidate does not solve the remaining
fragmented deep-tile or multi-native-tile preview GET counts.

`RIVER_S3_EXACT_SUPPORT=true` enables per-band inverse 5/3 dependency windows
for compatible zero-origin, single-native-tile, single-layer reversible Part1
images with usable packet lengths. A full-depth conservative margin can cover
an entire native tile on images with many resolutions; that previously disabled
spatial selection even for a small request. The per-band path retains spatial
demand in that case. Metadata and planning work stay bounded, and plans without
material precinct savings keep the previous mask. Source files and packet
entropy remain unchanged. Checkpoint 258 evidence against 253, including the
generated 12-resolution PLT fixture and progressive pixel checks, is under
`reports/exact-support-m254/`. Missing packet indexes, fragmented layouts, and
the remaining high GET counts are still separate performance limits.

## Portable two-image deployment

Use `Dockerfile.backend` and `Dockerfile.ui` for the slim deployment builds.
The existing `Dockerfile`/`compose.yaml` remain the development stack.
The portable backend bundles all three decoders, all four raster encoders,
and its loopback-only NVIDIA helper; no extra sidecar container is required.
Swagger UI is served offline at `/docs/`, with `/openapi.json` and `/swagger`.
Deployment templates and Windows/Linux launch scripts are under
`deploy/portable/`; see `deploy/portable/DEPLOY.md` for imagery mounts, cache
quotas, CPU/GPU startup and optional OTLP export.

```powershell
docker build --platform linux/amd64 -f Dockerfile.backend -t river/backend:2026.10.12 .
docker build --platform linux/amd64 -f Dockerfile.ui -t river/ui:2026.10.12 .
python tools/export-portable.py
```

The export tool packages tested images, checksums, source and license notices;
it expects the portable CPU/GPU validation reports to have been generated.

## Goal

Explore browser-first streaming of selected JPEG 2000 raster regions with progressive quality, explicit capability limits, and complete request evidence.

## Approach

River keeps source JPEG 2000 files immutable and uses content SHA-256 or explicit version identities plus technical sidecars. Per request, the viewer selects a decoder, delivery method, and output format independently; the browser renders the resulting tiles in WebGL and shows the selected driver and format beside the trace ID and timings.

## Status

Active

## Current vertical slice

- Compilable Rust `axum` service with an OTLP tracing path, a technical manifest endpoint, and a versioned binary tile-frame protocol.
- WebSocket viewport requests with discrete zoom, pass ordering (`preview`, `gain`, `final`), and limited-capability control messages.
- A pluggable `TileEngine` boundary with OpenJPEG, JP2Grok, and nvJPEG2000 selections. All engines return canonical RGBA tiles before the selected output encoder runs.
- Independently selectable Progressive WebSocket or WMTS delivery, plus WebP, PNG, AVIF, or JPEG XL output.
- A WebGL2 viewer with pointer-drag panning, cursor-anchored wheel zoom, a `Random view` corpus picker, side controls intended for automation/precise requests, and a per-frame latency waterfall.
- Local MinIO, Collector, Jaeger, Prometheus, Loki, and Grafana topology in `compose.yaml`.

The default asset is the immutable `Bretagne2.j2k` OpenJPEG corpus source, identified by SHA-256 `94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290`. Compose downloads it into a named volume and verifies that digest before River starts. No whole-slide/cell preview is bundled or substituted for unavailable sources.

## What River supports today

River serves **JPEG 2000 inputs**, including local `.j2k`/`.j2c`/`.jpc`/`.jp2` sources and objects discovered through a configured S3 source. S3 discovery also accepts native HTJ2K `.jph`/`.jhc` inputs. JPIP and HTJ2K are supported development directions, not prohibited technologies. Native HTJ2K range delivery is pixel-verified for lossless RGB8 and grayscale16 tiled PLT-indexed fixtures through JP2Grok and nvJPEG2000; other source-layout limitations still apply. OpenJPEG cannot decode HT blocks. S3 manifests report `codestreamCoding` and coding-compatible `supportedDecoderDrivers`, and the viewer selects an available compatible driver automatically. OpenJPEG and JP2Grok can convert supported source components to River's canonical 8-bit grayscale, grayscale-plus-alpha, RGB, or RGBA tiles. It supports:

- reference-grid viewport coordinates and power-of-two resolution reduction;
- region-of-interest decode through OpenJPEG (`decode_area`), without decoding the whole source for every tile;
- progressive `preview`, `gain`, and `final` raster frames over WebSocket;
- selectable derived WebP, PNG, AVIF, or JPEG XL tiles; these are transport formats, not source formats.
- per-frame timing exposure: blocking-worker queue, selected JPEG 2000 decoder, selected output encoding, server elapsed time, approximate wire/client queue, browser decode, and WebGL upload/draw.

The viewer exposes **Target decoder**, **Request method**, and **Output format** as separate controls. Decoder availability is reported by `/api/drivers`; output encoder availability is reported by `/api/formats`. OpenJPEG remains the portable default. JP2Grok uses the upstream `grk_decompress` region decoder, and the `grok` CMake build is AGPL-3.0. nvJPEG2000 runs in an isolated native nvJPEG2000 sidecar with Docker GPU access. Its tier-2 JPEG 2000 work remains CPU-side, while the later decode stages run on CUDA; it emits final-quality tiles rather than per-quality-layer refinements. The NVIDIA driver selector therefore remains independent from the WebSocket-versus-WMTS method.

River starts with the bundled 2,592 × 1,944 `Bretagne2.j2k` source. For a production-sized corpus, it reads an immutable, prebuilt `river-assets.json` catalog from `RIVER_ASSET_CATALOG`: startup and `/api/assets` then read only that small manifest, never crawl or hash 27 GiB of JP2 files. The ingestion job owns identity hashing, dimensions, and native-tile layout metadata, then atomically publishes a new catalog before a deliberate pod rollout. That layout record is only source technical metadata; it lets the direct engine reject an unsuitable native traversal without opening the JP2. Older catalogs remain compatible and use a header probe. Without a catalog River retains the local development discovery fallback.

## Production direction: direct source engines

River's required contract is **JP2 object + viewport = raster tiles**. It does not require a generated pyramid or a result cache. `direct-jpeg2000` is isolated from WebSocket delivery, catalog handling, and browser rendering. The S3 range engine now fetches bounded headers and selected original packets where the source layout permits it; this is not a JPIP wire-protocol implementation. A JPIP source adapter or delivery endpoint can share the engine boundary without replacing the raster protocol. The local engine makes a per-request, trace-visible choice: a small native-tile set uses OpenJPEG's native-tile entrypoint; a larger set uses bounded ROI decoding. The latter is not hidden as a “fast native tile” request.

The packaged image enables the in-process Grok bridge by default (`RIVER_GROK_NATIVE=false` restores the CLI-only path). Compatible S3 subsets decode from memory, avoiding a CLI process and intermediate files. Native rejection, including unsupported color conversions, falls back to the CLI; a memory subset is materialized only when that fallback is needed. ICC profiles, palettes and channel definitions are checked in the parsed header before native decoding, avoiding a duplicate decode before the CLI conversion path. The decoded output retains a second compatibility check. `RIVER_GROK_NATIVE_THREADS` controls the shared pool (default 2, range 1–64). Exact fixture coverage includes RGB8/grayscale16 legacy and native HTJ2K, reduced/full resolution, shifted origins, ROI and progressive delivery.

Large unindexed grids receive a bounded default allowance for validated tile-part discovery and required resolution-prefix reads. Explicit `RIVER_S3_REQUEST_RANGES` settings remain hard limits, and byte/deadline limits still apply. Coarsest preview navigation can include useful packet prefixes for grids up to 16,384 native tiles, avoiding a second round of small GETs. This trades some bounded extra bytes for fewer requests. Under nearby-source contention, speculative index probing is suppressed.

`RIVER_GROK_PACKED_VIEWS=true` enables entropy-preserving, reduced-resolution views for eligible complete lossless RPCL grids. The packaged image enables it; set it to `false` to restore the original native representation. It reuses original LL packet bytes without changing source images or public tile geometry. Unsupported layouts, color metadata, multiple layers, MCT and unaligned regions retain their existing path. The parser bounds input, output pixels and packet count. `RIVER_PROFILE=throughput16` already selects an eight-thread shared Grok pool for larger machines.


### JPIP and HTJ2K with immutable sources

Pure HT coding without PLT/TLM can now use inline resolution prefixes for supported LRCP/RLCP/RPCL layouts. The packet reader handles separate cleanup/refinement lengths and placeholder passes, following the [OpenJPH packet-header reference](https://github.com/aous72/OpenJPH/blob/master/src/core/codestream/ojph_precinct.cpp). Mixed HT/MQ styles and other unsupported layouts retain the bounded compatibility route. A whole-native-grid request can fuse its small prefix probe with lazy SOT discovery; partial requests keep tiny traversal probes and skip unselected entropy. In matched synthetic RGB8 fixtures, the first preview transferred 110,592 bytes instead of 2,676,209 bytes (about 96% less). Boxed/raw RGB8 and grayscale16 pixels match an independently encoded legacy oracle at three resolutions with CLI and native Grok. `tools/verify-native-ht.py --unindexed --progression LRCP|RLCP|RPCL` exercises this path; without `--unindexed`, it checks PLT/TLM fixtures. No original is transcoded by serving. On a fast high-latency link, fetching a small whole object may finish sooner than discovering many unindexed tile offsets; transfer reduction is not a universal latency-speedup claim.

Source objects must remain byte-for-byte unchanged. Technical indexes, original-byte range caches, temporary packet subsets, and rendered response caches may be disposable acceleration aids; none is a registration or preprocessing prerequisite. Cold source IO, index construction, repeat loads, pixel correctness, and concurrent-instance capacity must be measured separately.

[JPIP (JPEG 2000 Part 9 / ITU-T T.808)](https://www.itu.int/rec/T-REC-T.808) is an intended optimization path for existing JPEG 2000 files. View-window requests and client cache models can deliver missing header/precinct data incrementally. Plain S3 supplies object ranges, not JPIP responses: River would need a bounded, lazy packet index and a JPIP server/adapter. A server that first reads and indexes the entire object does not solve the cold-load requirement. Sparse source layouts and unsupported packet headers must retain explicit IO limits and visible fallback routes. The current WebSocket protocol delivers rendered raster frames; browser JPIP decoding would be an additional capability.

[HTJ2K (Part 15)](https://jpeg.org/jpeg2000/htj2k.html) replaces the legacy block coder. Fast native HT decoding is useful when the original already contains HT blocks; selecting an HT decoder cannot give legacy MQ-coded files the HT block-coder speedup. Lossless legacy-to-HT transcoding is possible but changes the compressed representation, incurs preparation cost, and is not a required or default serving path. Originals must never be rewritten. The existing derivative-cache experiment is opt-in and distinct from caching original bytes.

Research candidates checked on 2026-10-14: [OpenJPH](https://github.com/aous72/OpenJPH) is a BSD-2-Clause HT-only decoder candidate, not a replacement for the legacy decoder. [OpenHTJ2K's JPIP implementation](https://github.com/osamu620/OpenHTJ2K/blob/main/docs/jpip.md) documents precinct delivery, byte budgets, and session/cache models. Its example server loads a codestream and builds its index before serving, so it is a protocol/interoperability candidate rather than evidence of fast cold S3 access. Before integrating either, pin a revision, audit supported codestream features and licensing, and verify untouched-source pixels, first-preview bytes/latency, later refinement reuse, cancellation, and bounded concurrent memory. No River JPIP conformance or HT cold-load speedup is claimed yet.

`python tools/verify-native-ht.py --api URL --container river-goal-TEST --verify-capabilities` creates only new synthetic matched legacy/HT fixtures inside a task-owned test container. Pillow/OpenJPEG independently decodes the legacy counterparts as the preview/intermediate/detail pixel oracle. It checks paged discovery and manifest loading without explicit registration, boxed `.jph` and raw `.jhc`, RGB8 and grayscale16, low preview transfer, and rejection of OpenJPEG before HT payload reads. Pass `--driver nvjpeg2000` on a GPU-enabled test container to validate the NVIDIA path. No actual serving corpus source is transcoded.

Grok defaults to two worker threads per decode (`RIVER_GROK_THREADS_PER_DECODE`, clamped to 1–16), independently of River's decode admission limit, which the packaged sizing profile determines from CPUs and memory. This bounds decoder thread pools competing on smaller instances. Across three 2026-10-14 eight-distinct-client trials under a Docker two-CPU quota, the median trial cold-final p95 improved from 6.01 s to 4.51 s for small images and from 8.19 s to 5.00 s for large images, with identical source transfer. The two-thread small-image p95 varied from 4.35 s to 6.00 s. These are local protocol trials including S3 preparation and scheduling, rather than universal driver rankings or physical weak-PC measurements.

Manifests separately report `tilesAvailable`. A `limited` capability notice can describe bounded source IO or unsupported fast paths while the source remains renderable. The viewer uses tile availability to decide whether to render; it does not silently replace a working S3 image with the unrelated demo overview merely because a layout limitation was recorded.

`RIVER_ENABLE_DELIVERY_PYRAMIDS` defaults to `false`. The optional generated JP2 tiers below exist only as a migration/demo experiment and are never a production serving prerequisite. The direct engine currently uses OpenJPEG area decoding; it is not yet a claim that every arbitrary JP2 has efficient codestream-tile access. River must classify unsuitable layouts rather than hiding expensive work behind a cache.

`RIVER_MAX_NATIVE_TILES_PER_JOB` defaults to `4`. It is a CPU/I/O safety limit, not a cache: a request above that native-tile count is explicitly routed to the ROI decoder and marked `decoderRoute: "roi"` in its frame timing. A small native request is marked `decoderRoute: "native-tile"`. On the local 35-date, lossless 1024 × 1024-tiled corpus, the measured 2026-09-14 direct-source baseline was about 1.42 s for a 256 × 256 zoom-0 first pass (`native-tile`) and about 4.95 s for a 1920 × 1080 zoom-3 first pass (`roi`). These are current machine-local measurements, not performance claims or target compliance.

Every operational boundary emits nested OTLP spans: HTTP/catalog request, WebSocket session, viewport, decoder queue, tile engine, native-tile or OpenJPEG area decode, RGBA normalization, and WebP encoding. Pure hot arithmetic helpers deliberately do not make individual spans: their runtime is contained by the enclosing operational span, avoiding an unbounded trace-volume tax. The frame trace ID and selected decoder route are derived from that same span tree and exposed in the viewer under the `river-server` service.

## 30 GiB JPEG 2000 atlas in progress

`tools/download-sentinel-atlas.ps1` is a resumable downloader for a roughly 30 GiB test corpus made entirely of public Sentinel-2 Level-1C `.jp2` band files. Each acquisition combines the `B04`, `B03`, and `B02` JPEG 2000 components into RGB; four contiguous UTM 10S tiles form one east-west strip. Its generated `atlas.json` records every source URL and expected byte count.

Run it separately from Compose because the corpus is intentionally large:

```powershell
Set-Location streaming/river
./tools/download-sentinel-atlas.ps1 -TargetGiB 30
```

Compose mounts `corpus/cache/sentinel-atlas` read-only at `/app/sentinel-atlas`, but downloading it does **not** yet register it as a River asset. The pending JPEG2000-only source-layout resolver will map a virtual atlas viewport to the correct `.jp2` files and compose the three native bands into the streamed RGB raster. No conversion to a different input format is involved.

## 50 GiB diverse Sentinel add-on

`tools/download-sentinel-diverse-atlas.ps1` adds a separate, resumable corpus of
roughly 50 GiB. It excludes dates already present in `sentinel-atlas`, selects
one scene per acquisition date, and rotates through eight source positions in
four nearby tile pairs: west-coast `10SDG/10SEG`, central Europe
`31UDP/31UEP`, east America `17RKH/17RKJ`, and Asia `50QKE/50QKF`. The pairs
are about 100 km apart while the regional extremes are about 14,900 km apart.

Before any JP2 transfer, each candidate must meet the source coverage/cloud
limits and RGB quicklook checks (`meanLuma >= 40`, `p10Luma >= 8`, and at most
70% pixels below luma 25). The final `atlas.json` records every URL, expected
size, source ETag, tile center, and quality measurements; `evidence/` retains
the accepted `tileInfo.json` and `preview.jpg` files. The manifest is written
before downloading, so rerunning the command resumes incomplete objects and
finishes with SHA-256 and exact-size validation.

```powershell
Set-Location streaming/river
./tools/download-sentinel-diverse-atlas.ps1 -TargetGiB 50
```

This is raw one-tile-per-date JPEG 2000 input and is intentionally kept
separate from the existing per-date mosaic atlas; it is not automatically
registered as a River asset or converted to another input format.

### Exact-date grid companions

`tools/download-sentinel-grid-atlas.ps1` fetches one qualifying neighboring
MGRS tile for each primary scene when that neighbor exists on the same
acquisition date. It keeps the primary files referenced rather than copied,
so the companion corpus adds spatial grid coverage without introducing
duplicate primary dates. The grid plan is written separately under
`corpus/cache/sentinel-grid-atlas` and uses the same preview brightness,
cloud, coverage, exact-size, and SHA-256 checks.

```powershell
Set-Location streaming/river
./tools/download-sentinel-grid-atlas.ps1 `
  -SourceAtlas ./corpus/cache/sentinel-diverse-atlas/atlas.json
```

### Per-date JP2 mosaics

The atlas has 35 acquisition dates. `atlas-mosaic` combines all twelve source JP2 files for each date (four adjacent 10 m tiles × B04/B03/B02) into one georeferenced RGB JP2. It reads only JP2 inputs, creates virtual mosaics, then uses GDAL's OpenJPEG-backed `JP2OpenJPEG` driver with a reversible transform, 1024 × 1024 output tiles, and seven resolution levels.

```powershell
Set-Location streaming/river
docker compose --profile tools run --rm atlas-mosaic
```

Outputs are resumable at `corpus/cache/sentinel-date-mosaics/<date>.jp2`; completed files are checked with `gdalinfo` before they are skipped. `mosaic-manifest.json` retains the exact dates and all input objects. These large derived files are intentionally ignored by Git.

After the full-date mosaics exist, create the immutable serving catalog and two JP2-only delivery tiers:

```powershell
./tools/build-river-asset-catalog.sh
docker compose --profile tools run --rm gain-pyramid
```

The generated `river-assets.json` is about 10 KB for the 35-date corpus. `river-preview-jp2/` (1:16), `river-gain-jp2/` (1:32), and `river-thumbnail-jp2/` (1:64) are JPEG 2000 derivatives, not alternate input formats. They are used only when `RIVER_ENABLE_DELIVERY_PYRAMIDS=true`; the default direct-source deployment does not read or build them.

As each date finishes, use `Random view` to select a registered JP2 and random viewport. Use Refresh catalog to fetch a bounded page after startup discovery finishes; subsequent selections do not reread that file.

## Raw XYZ tile batches

Connect to `ws://127.0.0.1:18080/ws/tiles` and send:

```json
{
  "type": "tiles",
  "batch": {
    "assetId": "94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290",
    "sequence": 1,
    "tiles": [{ "x": 0, "y": 0, "z": 0 }, { "x": 1, "y": 1, "z": 0 }]
  }
}
```

`x` and `y` are zero-based **raster tile indices**, not source pixel positions.
`z` uses the existing River zoom model: `0` is full resolution, and a tile at
level `z` covers `512 × 2^z` source pixels per edge. These are asset-local
coordinates, not geographic XYZ/slippy-map coordinates. The endpoint requests
raw tile coordinates; source files remain registered immutable JPEG 2000 assets.

The response is exactly the viewport protocol: one `accepted` control message,
any `limited`/`error` controls, then binary `RIVR` WebP frames with the same
headers, trace/session IDs, timing fields, and available `preview`/`gain`/`final`
passes. `zoom`, `tileX`, and `tileY` identify each returned tile. There is no new
completion envelope. Track requested tile coverage per pass; a newer message
on the socket supersedes the previous request, and sequence IDs identify stale
frames that were already in flight. `/ws` also accepts this message; both paths
use the same session handler and decoder scheduler.

## WMTS tile service

River also serves OGC WMTS 1.0.0 KVP requests at `/wmts`. Fetch capabilities
with `GET /wmts?SERVICE=WMTS&REQUEST=GetCapabilities`, then request an individual
WebP, PNG, AVIF, or JPEG XL tile with `REQUEST=GetTile`, `LAYER=<asset ID>`,
`TILEMATRIXSET=RiverPixel-<asset ID>`, and matrix/row/column identifiers.
River's WMTS endpoint also accepts the vendor `DRIVER=openjpeg|jp2grok|nvjpeg2000`
parameter to select the decoder for that request.
Each tile is decoded through the same bounded direct JPEG 2000 engine and
shared worker scheduler. Responses include `X-River-Trace-Id`, decoder route,
and queue/decode/encode/server timing headers for tracing and measurement.

These matrices use River's image-local pixel grid because registered sources do
not currently include georeferencing. Capabilities identify this limitation;
the layer is usable as an image tile source but must not be overlaid as a
geographic layer. WMTS delivers one complete tile per request, so
it does not expose the progressive preview/gain/final passes from the WebSocket
mode. The viewer lets you choose a decoder, delivery method (Progressive
WebSocket or WMTS), and output format (WebP, PNG, AVIF, or JPEG XL) separately.
Tile edges are outlined over the image; change the outline color in the sidebar.
The sidebar shows the active request's trace ID, selected driver, and timing.

`node tools/measure-wmts-vs-progressive.mjs` runs paired, alternating warm
single-tile requests (set `RIVER_WMTS_ROUNDS` and `RIVER_ASSET_ID` to adjust).
On 2026-10-04, one 512 × 512 source-pixel tile from the default asset over five
measured pairs after one warmup per mode took WMTS PNG 227.5 ms median (p95
238.5 ms), versus 292.4 ms (p95 303.3 ms) for the progressive WebSocket final
frame. Both used the `native-tile` route. PNG was 383,167 bytes; WebP was 6,570
bytes, so the PNG tile was about 58× larger. This is a warm, loopback, single-tile
comparison, not a multi-viewer or cold-cache performance claim; the larger PNG
can make WMTS slower on real networks.

Each batch contains 1–256 entries for one asset, may mix zoom levels, and may
contain duplicates (deduplicated before decoding). The entire batch is validated
before acceptance; an invalid coordinate rejects it with `invalid_tiles`.
Tile indices must intersect the source; right/bottom edge tiles are clipped.

River may return **more unique tiles than requested**, as well as progressive
updates to each tile. It fills gaps within occupied 2×2 decode unions: the two
diagonal coordinates above yield all four tiles of that square. Extra tiles
never extend a union's requested bounding region, and expansion is at most
twice the unique input count per zoom. Sparse coordinates do not become one
large bounding decode. Full rectangular batches schedule the same direct-source
decode jobs as the equivalent viewport. Mixed zoom levels receive full preview
coverage before refinement begins.

Run the protocol checks and paired stress benchmark from `streaming/river`:

```powershell
node ./tools/measure-tile-batches.mjs
$env:RIVER_VIEWERS = '32'
node ./tools/measure-tile-batches.mjs
$env:RIVER_BATCH_PROFILE = 'sparse' # diagonal inputs, duplicate, and extra returned tiles
node ./tools/measure-tile-batches.mjs
$env:RIVER_VIEWERS = '10'
$env:RIVER_BATCH_PROFILE = 'native' # small native-tile requests across corpus assets
node ./tools/measure-tile-batches.mjs
$env:RIVER_BATCH_PROFILE = 'roi' # 1920x1080 at reduction 3 across corpus assets
node ./tools/measure-tile-batches.mjs
```

The benchmark verifies byte-identical WebP payloads against viewport requests,
all-pass coverage, extra tiles, duplicates, mixed zoom, edge clipping, rejection,
and supersession. It warms both paths, alternates paired load-wave order, and
fails if batch p95 exceeds viewport p95 by more than 20% (override with
`RIVER_BATCH_MAX_RATIO`). `RIVER_ROUNDS` defaults to 3; `RIVER_SEED` makes
coordinates reproducible. JSON reports retain workloads, samples, and trace IDs;
Markdown summaries are written under `reports/tile-batch-*`. Performance covers
local WebSocket delivery with the shared direct-source engine and warm OS file
cache; it excludes browser rendering and remote storage. The 20% parity gate is
separate from River's absolute 250 ms target.

### Batch verification: 2026-09-15

Local release-container results with delivery pyramids disabled, 32 shared
decode slots, four jobs per request, and one OpenJPEG worker per native decode.
Each comparison used three measured waves after warming both paths, with
alternating endpoint order and seed `20260915`. All five parity gates passed,
covering **564 measured requests**:

- HD sample, 10 viewers: viewport p95 **192.78 ms**, batch **180.88 ms** (0.938×).
- HD sample, 32 viewers: viewport **539.43 ms**, batch **553.49 ms** (1.026×).
- Sparse sample, 32 viewers: viewport **160.24 ms**, batch **163.19 ms** (1.018×).
- Native-tile corpus, 10 viewers on 10 assets: viewport **5,193.35 ms**, batch **5,142.92 ms** (0.990×).
- Large-region corpus, 10 viewers on 10 assets: viewport **29,350.41 ms**, batch **29,663.49 ms** (1.011×).

The pre-change viewport sample p95 was **194.60 ms**; the existing viewport
fixture also passed after the change at **187.98 ms**. Both host and Linux
release workspace tests passed (10 tests), along with formatting and Compose
configuration checks. Live protocol checks confirmed exact WebP payload parity,
coverage and rejection behavior, and cancellation; a batch trace was retrieved
from Jaeger with nested engine/codec/encoding spans. The large-source workloads
remain above the absolute 250 ms target despite matching viewport performance.

Detailed workloads and per-request trace IDs are retained in the local
`reports/tile-batch-{sample,sparse,native,roi}-*.json` files with Markdown
summaries. These are machine-local measurements, not browser or remote-storage
performance claims.

## Run

Docker is sufficient; no host Rust install is required.

```powershell
Set-Location streaming/river
docker compose up --build
```

Open [River viewer](http://127.0.0.1:18765), [River API](http://127.0.0.1:18080), [Jaeger](http://127.0.0.1:18686), [Grafana](http://127.0.0.1:18300), and [MinIO](http://127.0.0.1:18001). The stack uses the dedicated `18000–18999` host-port range; container ports and internal service-to-service traffic stay unchanged. The viewer explicitly targets the local River API and WebSocket on port `18080`. Local credentials are intentionally development-only: `river` / `river-local-only-change-me`.

## Runtime S3-compatible JP2 sources

River exposes a bearer-token protected runtime API for Amazon S3 and self-hosted
S3-compatible endpoints (including MinIO). The viewer's **Runtime S3 / MinIO
sources** panel configures credentials, lists JP2/J2K objects, and loads a
selected object into the normal viewer. The endpoint URL must be reachable from
the River container (for the Compose MinIO service, use `http://minio:9000`, not
the host's `127.0.0.1` address). Use path-style addressing for MinIO. Plain HTTP
must be explicitly enabled and should only be used on a trusted local network.

The HTTP surface is:

- `POST /api/s3/sources` to add/update a named connection; `GET /api/s3/sources`
  returns configuration without secrets; `DELETE /api/s3/sources/{id}` removes a
  runtime connection.
- `GET /api/s3/sources/{id}/objects?prefix=...` to browse object keys.
- `POST /api/s3/sources/{id}/assets` with `{"key":"folder/image.jp2"}` to load an asset with automatic metadata detection.

River development APIs require no administrator token. Configuration export includes full stored S3 credentials.

Tile-part and packet indexes are optional. If selective reads exceed their budget, or subset assembly cannot represent the native tile grid, River falls back to the native decoder on a disk copy (`s3-disk-compatibility`). This slower cold route reads version-pinned chunks of at most 1 MiB, uses a 300-second transfer deadline, and honors cancellation. It does not allocate an object-sized memory buffer or modify the original. The default maximum compatibility object size is 2 GiB (`RIVER_S3_COMPATIBILITY_BYTES`); concurrent transfers share source IO admission. A shared process-local LRU retains at most 2 GiB of completed copies (`RIVER_S3_COMPATIBILITY_CACHE_BYTES`, zero disables retention), keyed by source snapshot. Later tiles and quality passes reuse the copy without retrying selective reads. Concurrent misses for one object share a transfer. Eviction deletes the owned temporary file after any active decoder leases finish; active transfers and evicted decoder leases can additionally occupy disk within the source IO concurrency limit. Sources exceeding the configured disk limit receive an explicit resource-limit error, not an instruction to add indexes.

Connected S3 sources can be searched through `/api/assets?source=ID&keyPrefix=folder/image` or `/api/s3/sources/ID/objects?keyPrefix=folder/image`. The prefix is relative to the configured source prefix and is passed to S3 without appending a slash, so partial filenames work. `q=substring` performs case-insensitive key matching. Catalog search collects matching results through bounded resumable listing scans; direct object browsing filters one provider page. Direct objects browsing issues one bounded listing per request; catalog search uses bounded resumable matching-result scans as described below. In the viewer, select a data source, enter a search, choose **Key starts with** or **Key contains**, and use the next-page button for further results.

Every compact milestone image includes the API and the viewer. After `docker load -i CHECKPOINT.tar.gz`, run the exported tag with `docker run --rm -p 18080:8080 IMAGE_TAG`, then open `http://localhost:18080/`. The viewer, scripts, and API use the same container port; a separate UI image is unnecessary.

Non-zero JPEG 2000 image and tile-grid origins are supported by OpenJPEG and JP2Grok while River requests remain image-local. Metadata preserves the codestream canvas origin as `referenceOrigin`; packet selection clips native tiles in that reference grid. OpenJPEG regions are translated to absolute reference coordinates, while Grok windows stay image-relative. Compatible layouts keep packet-selective S3 reads, including all five indexed progression orders and unindexed LRCP prefixes. OpenJPEG uses ROI traversal for these layouts; nvJPEG2000 is omitted from their manifest/capability drivers because its shifted-origin rendering has not been verified. Original files are unchanged. Run `tools/verify-s3-ranges.py --origin-reference-container river-goal-ORACLE` for independent Grok-oracle checks, and `tools/verify-local-discovery.py --shifted-origin` for local full/reduced pixel and restart checks.

Tile-length tables are useful even when they omit tile identifiers. River derives bounded SOT offsets from those lengths, discovers identifiers in pages of at most 16 headers, validates each length/identifier against the actual SOT, and persists progress in the existing versioned technical-index cache. Cached SOTs are processed before prefetching another page. Explicit-ID tables overlap only selected headers in batches of at most 256 (anonymous tables retain 16-header discovery pages); discovery reads no unselected tile bodies. Tables are capped at 100,000 parts, extents are checked against the codestream rather than trailing JP2 boxes, and the ordinary request byte/range/deadline limits still apply. In the 48-tile anonymous-TLM synthetic fixture with 10 ms simulated range latency, complete preview time changed from 1.36 s to 0.85 s with identical source bytes and exact pixels. This is a layout-specific trial, not a universal timing claim. The range verifier checks both table styles and a fresh partial-region identity against an unchanged pixel oracle.

Selected PLT packets can also be prefetched across native tile parts in one bounded plan, instead of serial per-part payload requests. Admission caps are 4,096 parts, 16,384 selected spans, 2 MiB of useful packet bytes and 4 MiB of aligned required blocks. Larger plans keep the ordinary per-part reader. Only required blocks are leased, merging with existing leases within the 8 MiB request-retention bound, so a small shared cache cannot discard prefetched payload before assembly. Overall read/overfetch/range/deadline limits remain authoritative. In the native Windows 48-tile anonymous-TLM fixture with 10 ms simulated range latency, preview coverage changed from a saved 876 ms baseline to 297 ms; aligned transfer rose from 133,120 to 161,792 bytes within the existing overfetch policy. Pixels, cold partial-region transfer, cancellation and repeat-cache reuse passed. Checkpoint 60 passed the Linux/Grok range verifier, index-free HTJ2K CPU and indexed HTJ2K GPU pixel checks, S3 browsing and viewer search checks. On the same Linux engine with 10 ms simulated range latency, explicit-TLM coverage changed from 745 to 227 ms and anonymous-TLM coverage from 729 to 234 ms; these are single-fixture measurements, not universal speedups. The 979 MB corpus preview matched an independent pixel oracle while reading 750,818 bytes; repeated previews used no source reads. The old checkpoint images/exports were removed on 2026-10-15. The replacement deployable API/UI bundle is dist/river-deploy-2026.10.15-deploy-m61.zip; it includes Compose, CPU/GPU launch scripts, deployment notes and checksums. Its image retains the validated checkpoint 60 application code.

Remote IDs are `s3v-<digest>`, where the digest identifies endpoint/bucket/key and the HEAD version/ETag metadata. They are deliberately distinct from local content SHA-256 identities. Requests send version IDs and/or If-Match, validate range extents, and reject incomplete responses. Reconnect the source to discover a changed object; cached reads remain associated with the original snapshot.

Legacy local mosaic discovery defaults to `filev-<digest>` identities based on canonical path, file size and modification time, so discovering a large file does not first read it all to compute a content hash. These are file-version identities, not content SHA-256; manifests mark `identityKind: file-version`. They remain stable across restart while that metadata is unchanged. Existing supplied catalog IDs and the verified default asset retain their IDs. For compatibility with old automatically discovered content-hash URLs, set `RIVER_LOCAL_IDENTITY=sha256`, which reads full files during discovery. `version` is the default; other values fail startup. Reload the catalog when switching modes. `tools/verify-local-discovery.py` verifies discovery, restart stability, actual metadata, exact WMTS pixels and the compatibility digest on a disposable file with a 32 MiB trailing free box.

Packet-length indexes (PLT) permit exact resolution/layer selection for all five base progression orders with common zero-origin, unsubsampled component grids and ordinary packet headers. Single-layer LRCP/RLCP/RPCL streams also support prefix indexing without PLT with ordinary codeblock coding; inclusion and zero-bitplane tag trees are parsed without reading later resolutions. Other layouts use explicitly reported, bounded tile-part reads; particularly interleaved sources can require much more IO for a preview. Missing layouts, request budgets, or unsupported coding are reported rather than silently downloading unlimited data.

The OpenJPEG path decodes compact subsets directly from memory. File-based drivers receive temporary compressed subsets in `RIVER_S3_CACHE_ROOT/range-views`, removed after decode; this is not a raster pyramid or persistent full-object cache. WebSocket preview/refinement requests share original compressed blocks, and subsequent refinement fetches additional data only while that viewport remains current. The trace shows S3 bytes/ranges/cache reuse in both WMTS headers and WebSocket `serverTiming`.

## Baselines

The CUDA sidecar uses the native nvJPEG2000 tile/resolution API. Reduced tiles
use JPEG 2000 wavelet reduction, not full-resolution decode plus resizing.
Eight persistent workers each own independent parsed streams, CUDA streams,
and a bounded pool of decode states; no state is reused before completion.
Four tile streams overlap independent native tiles. Each state has its own
decode-area parameters; only the intersection with the requested ROI is
downloaded and converted. Coordinates use the native reduced-resolution grid.
`RIVER_NVJPEG2000_TILE_STREAMS` (1–8) and `RIVER_NVJPEG2000_PRECISE_ROI` allow
controlled ablations. Exact pixel checks cover native and reduced ROIs.
Eligible lowest-resolution previews use an exact compressed-packet projection:
only original LL packets are read, while SIZ/COD/QCD and tile-part lengths
describe their native reduced grid. This supports single-layer reversible RPCL
codestreams with one codeblock per lowest-resolution precinct and ordinary
coding style. Unsupported layouts keep the normal decoder. Original files are
unchanged; the bounded 16 MiB cache contains compressed bytes, not decoded
rasters. Such previews report `compressed-view` and do not enqueue whole-file
staging. `RIVER_NVJPEG2000_RESOLUTION_VIEWS=false` disables this optimization.
The sidecar span records view construction time and actual source bytes read.
For aligned power-of-two LL tiles with one codeblock per packet, their original
entropy bytes are regrouped in raster/precinct/component order into a single
zero-DWT tile. Precinct and codeblock dimensions preserve the original block
boundaries; no pixels are decoded or re-encoded. The 451-tile catalog overview
therefore becomes one GPU tile submission. `RIVER_NVJPEG2000_PACKED_VIEWS=false`
retains the original compact multi-tile projection for comparison.
Admission uses oldest-first bounded microbatches, matching source, dimensions,
and reduction. Fine ROIs additionally require identical native tile footprints
to prevent enlargement across unrelated tiles. `RIVER_NVJPEG2000_BATCH_REQUESTS`
(default 8) and `RIVER_NVJPEG2000_BATCH_WAIT_MS` (default 1, capped at 5) control
coalescing; aggregate batch outputs are bounded. Each request's output remains
separate. Shared decode stage timings describe the entire batch, not a duration
that can be summed across its requests. Trace attributes include batch request
and output counts. Each worker retains at most two parsed sources so an overview
does not evict its full source; neither is a decoded image cache.
Workers reserve 8 MiB each of GPU output and pinned download storage at startup
(`RIVER_NVJPEG2000_RESERVE_BYTES`) to avoid serving-time allocation growth on
normal tiles. Integer normalization uses exact precision-specific lookup tables.
PNG uses fast lossless compression; response byte counts may increase.
Catalog-based LL tiles prime CUDA state at startup before health becomes ready; health
reports this one-time warmup duration. `RIVER_NVJPEG2000_WARMUP=false` provides
an explicit process-cold comparison. It does not stage entire source files.

Compact images now include three generated 64x64 decoder probes (RGB8 legacy, grayscale16 legacy, and RGB8 HT), totaling about 17.5 KiB with their manifest. When present, the helper queues bounded warmup tasks before its persistent serving loops and listens without waiting for those tasks to finish. Its private health response reports `warmup.status`, `workersReady`, fixture names and elapsed time; GPU requests arriving during warmup may wait for a worker. Every worker decodes only the three tiny files at reduction 2 with a 4,096-pixel limit. No S3 request, user-image scan or original-file conversion occurs. CPU-only startup does not launch the helper. Existing catalog-based warmup remains the fallback for deployments without packaged probes. The disable switch still applies. A fresh-container on/off check after the machine's GPU driver cache was warm did not show a reliable latency difference; this moves decoder preparation into startup rather than proving an end-to-end cold-machine speedup. `tools/verify-gpu-warmup-scheduling.py` checks non-blocking task scheduling and file/decode bounds without requiring a GPU.
`RIVER_NVJPEG2000_WORKERS` adjusts the worker count (1–8). The private response
is length-prefixed JSON metadata plus binary RGBA, avoiding base64 transfer.
The output pixel limit applies after reduction, and native workspace is also
bounded (`RIVER_NVJPEG2000_MAX_NATIVE_BYTES`, default 128 MiB per worker).
This native path supports unsigned 1–16 bit, equally sized component planes.

On Docker Desktop, random reads from Windows bind mounts can dominate mosaic
latency. Compose enables `RIVER_LOCAL_SOURCE_CACHE_ROOT` on its Linux named
volume. Original codestreams are copied lazily, checked with SHA-256, and
published atomically; decoded rasters are never cached. The default
`RIVER_LOCAL_SOURCE_CACHE_BYTES` budget is 2 GiB. Inactive copies are evicted
first, active reads retain a lease, and oversized sources use their original
path. Cold requests decode directly from the original immediately. Only after
successful raster preparation is one background fill enqueued per source;
there is one copier and at most four distinct queued sources. Foreground
lookups never hash, copy, or wait for verification. Bulk IO does not hold the
cache catalog lock, so ready images remain usable during another source fill.
Persisted copies are reverified in the background after restart. The complete
copy/checksum cost remains in `river.source.cache.fill` / `prepare` traces;
HTTP headers and frame timing expose `sourceCacheStatus` and foreground lookup
time. `warming` means original bytes are serving while staging continues.
Cache errors fall back to originals.

Fine nvJPEG2000 requests use persistent Rust CUDA workers when the original
native tile structure permits an exact standalone compressed tile view.
Unsupported layouts fall back to the sidecar. Each thread owns its decoder
states and stream; compressed tile bytes remain alive through synchronization.

Optional lossless HTJ2K derivatives are an opt-in disposable compressed-cache experiment. Originals
are never edited. `RIVER_HT_CACHE_BYTES` defaults to zero, disabling admission
and lookup. An explicit positive budget enables transcoded cache representations;
it is not needed for native HT input decoding. `RIVER_HT_CACHE_ROOT` selects the directory,
and `RIVER_HT_CACHE_WORKERS` defaults to two (maximum four). Misses serve original
codestreams and enqueue bounded, low-priority background preparation. Admission
requires exact RGB16 agreement at full resolution and reductions 2 and max;
currently only reversible, unsigned RGB16 sources without MCT are eligible.
Persistent compressed bytes use LRU eviction and checksum validation on load.
The budget covers admitted compressed payloads, excluding small manifests,
bounded temporary build files, and decoder working memory. Preparation can still
compete for CPU/IO, so cache-hit timings must be reported separately from misses.
`GET /api/cache/ht` exposes usage, entries, pending jobs, hits, misses, failures,
and cumulative preparation time. Trace `ht_cache_tiles` and decoder route
`nvjpeg2000-ht-native` distinguish admitted derivatives from original decoding.

The viewer uses a same-origin proxy, six center-prioritized WMTS requests,
and a native overview while detail refines. Panning admits newly visible tiles
immediately when the visible tile set changes. Overlapping requests/textures
are retained; queued offscreen work is skipped, and asset/zoom/format changes
cancel the previous mode. Progressive dragging requests missing visible tiles
through full-quality tile batches, rather than waiting for idle refinement.
Trace requests and their frame rows append to history for the page session,
with horizontal separators and automatic scrolling in the same bounded panel.
The former “Wire” value included hidden browser socket queuing and dispatch;
it was not pure network transfer time. The UI now separates its own request
queue and labels the residual as a transport/dispatch estimate. Browser decode,
texture upload, and animation-frame wait are reported separately. Presentation
timings end at frame submission, rather than measuring physical display latency.

`node tools/measure-wmts-drivers.mjs` measures all available drivers, PNG/WebP,
sequential pinpoint tiles, and 8/32-worker bursts, retaining exact trace IDs.
Set `RIVER_ASSET_ID`, `RIVER_PINPOINT_COUNT`, and `RIVER_BURST_COUNT` to reproduce
the mosaic workload. `tools/verify-wmts-raster.py` runs inside the sidecar and
checks decoded dimensions and PNG pixel agreement at reductions 0, 2, and max.

The corpus URLs and SHA-256 identities are tracked in [`corpus/manifest.json`](corpus/manifest.json). Compose obtains and verifies the default source automatically; the optional host baselines use `corpus/cache/`.

```powershell
./tools/measure-openjpeg-baseline.ps1
node ./tools/measure-protocol-fixture.mjs
node ./tools/measure-random-corpus-load.mjs
node ./tools/measure-interactive-churn.mjs
```

The first command measures a single-threaded full-image FFmpeg/OpenJPEG decode. The second drives a fixed default-image workload through the WebSocket. The third drives deterministic random assets and random viewports from the registered corpus. It fails when p95 first-pass latency exceeds `RIVER_TARGET_P95_MS` (250 ms by default). Set `RIVER_COMPLETION_PASS=gain` and a suitable `RIVER_TARGET_P95_MS` to gate the sharper progressive tier too. The fourth sends 12 same-zoom drag positions at the viewer's 60 ms cadence and fails when the final position does not receive full first-pass coverage within `RIVER_CHURN_TARGET_MS` (250 ms by default). These protocol scripts measure server-to-WebSocket delivery; they exclude browser image decode/WebGL upload and object-store reads. Set `RIVER_VIEWERS`, `RIVER_SCREEN_WIDTH`, `RIVER_SCREEN_HEIGHT`, `RIVER_ZOOM`, and `RIVER_SEED` to reproduce a corpus run.

## Success criteria

- At 1920×1080 CSS pixels, ten loopback browser viewers achieve full preview coverage at p95 under 250 ms for fast-path assets.
- Every public corpus asset either meets the fast path or has a concrete, recorded limited-capability reason.
- Every viewport request has retained trace/ledger evidence plus a JSON and Markdown benchmark result.

### Bounded browsing and automatic S3 loading

After connecting a source, its JPEG 2000 objects appear in the viewer's data-source selector without ingestion or manual registration. Select an object to detect its codestream metadata and load it. Browsing never downloads objects or scans the entire bucket: `/api/assets?source=ID&limit=50&cursor=TOKEN` and `/api/s3/sources/ID/objects` make one S3 ListObjectsV2 request with a default of 50 and maximum of 100 keys. Responses contain `objects` and `nextCursor`; a page may contain no supported images while still having a next cursor. Keep the same source and prefix when following cursors.

Local `/api/assets?limit=50&cursor=ASSET_ID` preserves the manifest-array response and returns `X-River-Next-Cursor` when another page exists. Search and sorting in the viewer apply to the current page. Normal catalog reads do not start storage crawls; the non-catalog development fallback runs once at startup; legacy `refresh=true` list requests are also bounded.

Source configuration and removal require no token. Browsing configured sources, loading their allowed-prefix objects, manifests, and tiles follow the public catalog access policy. `POST /api/s3/sources/ID/assets` requires only `{"key":"folder/image.jp2"}`; dimensions and bit depth are optional validation hints. Connections and loaded-object mappings remain in memory. Sources should contain immutable objects; reconnect a source to invalidate its loaded mappings.

S3 selection reads headers only. On-demand packet or tile-part reads serve the requested viewport; cached compressed ranges are shared by later passes and clients. CPU decode defaults are two concurrent jobs, configurable via `RIVER_DECODE_CONCURRENCY`. UI proxy reads allow ten minutes, while source range reads have their own shorter budgets. Tile errors leave other tiles loading and display the actual source failure.

### Compact milestone images

WMTS GetTile accepts an optional vendor `LAYERS` count for OpenJPEG/JP2Grok. Omission or zero retains full-quality behavior; unsupported counts and NVIDIA layer-selection requests return 400. The viewer's coarse WMTS overview uses `LAYERS=1` for compatible CPU drivers, then loads ordinary full-quality detail tiles. Encoded cache keys include the layer limit. On the unchanged small corpus source, a first-layer PNG WMTS overview fetched 222,599 bytes, matched independent pixels, and took 224 ms in a local cold trial; the repeat used zero source IO. `tools/benchmark-s3-corpus.py --objects small --wmts-preview --api URL` verifies this route.

The standalone image accepts `RIVER_PROFILE=auto` (default), `compact`, or the opt-in `RIVER_PROFILE=throughput16` at container startup. The latter sets eight decode slots, sixteen source-IO slots, one OpenJPEG thread per decode, two CLI Grok threads per decode, eight native Grok workers shared across the process, and 256 MiB each for compressed-range and encoded-raster payload caches. Explicit environment settings take precedence, including those provided by Compose; the profile does not reserve CPUs or change source files. It is a starting point for machines with substantial CPU/RAM, not automatic hardware detection. For example, add `-e RIVER_PROFILE=throughput16` to the `docker run` command for the milestone tag. Direct Rust binary launches use the corresponding individual environment settings.

The 2026-10-14 unrestricted Docker VM reported 22 logical CPUs and 11.37 GiB RAM. With sixteen distinct source identities, the packaged throughput profile reduced small-image cold-final p95 from 6.07 s to 3.21 s and large-image p95 from 8.57 s to 7.05 s in a local protocol trial. Small-image source transfer fell from 258.6 MB to 86.2 MB: the larger range cache avoided eviction/refetch across progressive passes. Large-image transfer stayed 50.0 MB, and repeat loads performed zero source IO. Increasing workers alone, while retaining 64 MiB caches, left small-image transfer at 236.6 MB; cache capacity must match the concurrent working set. These are individual trials with fresh River identities, a warm OS file cache, and no browser-paint measurement; the CPU topology is not a claim of sixteen equivalent physical cores.

`tools/build-milestone.ps1 -Milestone 2026.10.14-range-m1` freezes source inputs, records SHA-256 hashes, and builds the multi-stage backend as `river:<milestone>`. The runtime contains the API, viewer, OpenJPEG/JP2Grok and optional NVIDIA support in one image; compilers and build outputs remain in builder layers. Build manifests, status and logs live under `reports/image-builds/<milestone>/`. Run the script in a background process/session while independent work continues.

The image serves the viewer and API on port 8080 and starts with the verified bundled sample without requiring a catalog. Mounting an explicit catalog is still supported through `RIVER_ASSET_CATALOG`; the portable Compose configuration enables `/data/river-assets.json`. Bind the host to the River port range, for example `127.0.0.1:18080:8080`. Runtime configuration APIs require no token. Add GPU access only on a compatible NVIDIA host.

Validation: `cargo test -p river-server`, `python tools/verify-s3-browsing.py`, and `python tools/verify-s3-ranges.py`. The latter checks real encoded 8/16-bit fixtures, all five progression orders, indexed and unindexed prefix paths, intermediate/detail pixels, S3 transfer accounting, repeat reads and WebSocket preview pause/refinement/cancellation. Its loopback timings are not a claim of cold filesystem or production latency.

Encoded rasters also have a shared, 64 MiB in-memory LRU (`RIVER_RASTER_CACHE_BYTES`, zero disables it). Its key includes source snapshot, driver, format, resolution, quality, tile geometry and derivative path. Hits bypass source IO, codec work and the shared decode semaphore; traces report `encoded-raster-hit` and zero decode/encode/source-read work. Entries do not persist across process restarts. Small unindexed/multi-layer objects may batch required bytes when the requested view covers every native tile; indexed objects and partial views continue selective reads.

Entries are per output tile, so a tile rendered in a WebSocket batch can be reused by an individual WMTS request. Full-quality requests normalize the complete source layer count and `LAYERS=0` to the same decode selection; partial quality stays separate. CPU/Grok batches retain encoded hits and prepare/decode only their missing tiles, then return all requested rasters. A partially cached whole viewport is treated as a partial source read. GPU engines requiring a complete native tile grid retain their original batch geometry to avoid turning cache holes into whole-source compatibility downloads. `tools/verify-s3-ranges.py` checks cross-protocol exact pixels and mixed cached/missing batches, including their decoder routes.

Unindexed lowest-resolution packet probes start at 64 bytes and grow when the packet header needs more space, up to the existing 1 MiB header bound. Dependent LL packet headers are resolved in rounds across selected tile parts, followed by concurrent reads of only their required packet prefixes. Plans are bounded to 4,096 tile parts and 16,384 prefix packets; larger plans keep the existing reader. This avoids speculative 1 KiB probes crossing into unused blocks while retaining long-header compatibility. In one fresh-identity 979 MB corpus preview comparison with 10 ms simulated latency per range, checkpoint 54 reduced complete preview coverage from 8.24 s to 5.51 s, 935 to 734 ranges, and 956,642 to 750,818 source bytes. First-frame time changed from 6.44 s to 5.42 s. OS caches were not flushed and browser decode/paint is excluded; sequential initial tile-part discovery still dominates this unindexed source.

The large-corpus PNG preview was also compared exactly against Grok decoding the original file directly into a 16-bit PPM at reduction 6, layer 1. `tools/benchmark-s3-corpus.py --objects large --verify-preview-pixels --preview-reference PATH --reference-reduction 6` checks such an independent P6 PPM oracle, normalizing its samples to River's 8-bit output policy. This verifies a preview, not the full-resolution source. The synthetic range verifier includes a two-resolution, 4x4-codeblock fixture whose packet headers exceed the initial probe size.

Concurrent identical cold renders share a weak per-key flight gate before source IO or CPU admission, then recheck the encoded cache. Different snapshots, drivers, formats, resolutions, quality levels and tile sets remain independent. The blocking decode worker retains the gate through cache insertion, including when its caller disconnects. Failed renders release the gate without caching a failure. Setting the raster-cache budget to zero also disables render coalescing. Weak gate entries are pruned on subsequent misses. `tools/verify-render-coalescing.py --api URL` checks a fresh instance with 16 concurrent requests to one native WMTS tile, requiring one decode response, byte-identical outputs and zero repeat decodes. Existing workloads may already avoid duplicate work through earlier cache rechecks; this is not a universal latency-speedup claim.

Remote preparation has independent admission (`RIVER_SOURCE_IO_CONCURRENCY`, default 4). Source IO does not occupy the two default CPU decode slots. Admission stays held while prepared requests wait for a CPU slot, preventing an unbounded queue of compressed buffers. Encoded hits bypass both schedulers. Individual S3 bodies are bounded while streaming, not merely checked after collecting a response.

When a small fallback requires the entire compressed object, preparation leases its already-fetched blocks until subset assembly completes (at most 8 MiB per active request). Reads and header-prefetch checks consult those leases before the shared LRU, preventing other objects from evicting bytes that this request still needs and causing duplicate downloads. The leases end with preparation; partial/indexed views do not trigger full-object pinning. With a 1 MiB range cache and a 6.3 MB per-job read limit, the 5.39 MB sample previously exhausted its budget through refetches; the leased path completes preview/final delivery under the same limit. Separate passes may still refetch when the configured cache cannot hold their working set.

Header-only S3 opens share that IO admission and coalesce per object. A blocked HEAD for a new object does not hold up a cached manifest or an unrelated object. Configuration changes take an exclusive gate so an old open cannot repopulate invalidated mappings; ordinary opens share the read gate. Weak per-object gates are reclaimed as requests finish, and blocking header readers retain their IO permit even if the caller disconnects. With 100 ms simulated HEAD latency and 16 distinct concurrent tiny-image opens, the 2026-10-14 local trial reduced metadata-stage time from 1.72 s to 0.47 s with identical source bytes. `--parallel-open --head-latency-ms 100` reproduces this workload in the corpus benchmark.

Refinement's idle gate applies only to cache misses and is shared across the viewport's quality passes. Cached final/gain tiles are delivered immediately; after the first uncached refinement waits for idleness, subsequent passes add no new pause. NVIDIA's stream parser requires complete tile grids: the range adapter can provide a cropped rectangular grid of power-of-two native tiles, updating SIZ/IHDR and tile identities without re-encoding entropy bytes, then restores original transport coordinates. Other partial geometries remain explicitly limited for NVIDIA; OpenJPEG/JP2Grok support the ordinary sparse subset path.

After lazy tile-part discovery, compatible unindexed resolution-prefix probes overlap up to four range requests. Distant prefixes stay separate rather than fetching the high-resolution payload between them. Prefix read-ahead is 1 KiB for resolution 0, 8 KiB for resolutions 1 and 2, and 64 KiB above that. The TCP listener enables `TCP_NODELAY` so small progressive/cache-hit frames do not wait for delayed acknowledgments.

Unindexed LRCP sources can also deliver a reduced first-quality-layer prefix without fetching the later layers. Tile COC markers are accepted for packet planning only when the component coding/precinct geometry exactly matches COD; other overrides retain the explicit fallback. Component quantization markers remain in the temporary subset. Read-ahead is now 8 KiB for resolutions 1 and 2. On the unchanged 5,386,631-byte `Bretagne2.j2k` corpus sample, a fresh progressive preview fetched 222,599 bytes rather than the full file, with exact PNG pixels against independent reduced/first-layer decoding. Local preview coverage was 105 ms with OpenJPEG and 56 ms with Grok in individual trials; subsequent final refinement brought total source transfer to one original object, and repeat requests performed zero source IO. These protocol measurements exclude browser paint and do not imply efficient arbitrary multi-layer access without PLT. `tools/benchmark-s3-corpus.py --objects small --verify-preview-pixels --api URL` verifies the preview using PNG; it accepts only small preview-only corpus objects and assumes the default two extra preview reductions.

Compatible PCRL/CPRL layouts use 256-byte cached ranges; other layouts and header detection use 1 KiB blocks. Cache identities are shared rather than copying the version string into every block, and block-size namespaces prevent alignment collisions. Packet-range merging aims for fewer requests but defaults to at most twice the uncached aligned useful bytes (`RIVER_S3_RANGE_OVERFETCH_RATIO`, clamped 1–16), while retaining the overall job byte/request limits. A heap of neighboring gaps avoids quadratic rescanning on dense plans; regression checks compare its output with the original scan planner. Small interleaved fetch plans (at most 1 MiB) retain their sixteen-range default. Ordinary compact plans (at most 1 MiB total, no individual range above 8 KiB) use sixteen only when the fastest observed successful source range takes at least 2 ms; fast or unmeasured sources default to four. The minimum is learned from successful header and payload reads for the source snapshot, reducing the influence of handshake or queue spikes. Larger transfers remain at four unless the large-grid parallel-task policy described below applies. A rolling window admits replacements as each response completes, so a slow response does not hold up an entire group; completed data is cached immediately without collecting a whole plan in memory. `RIVER_S3_RANGE_CONCURRENCY` overrides that choice (1–16). Source IO admission remains a separate instance-wide bound, so raising both controls raises the possible number of simultaneous source requests.

The viewer aborts obsolete manifest requests and individual off-screen WMTS downloads while preserving overlapping tiles during same-zoom pans. Blocking source preparation owns both its IO permit and duplicate-render lock until it exits, even if its caller disappears. Superseded requests are checked before preparation and again before entering the decoder; this does not interrupt native decoding already in progress.

WMTS browser admission defaults to two requests. For bounded client tuning, open the viewer with `?wmtsConcurrency=4` or set `window.RIVER_WMTS_CONCURRENCY` before loading its script; values clamp to 1–6. This does not raise the server's source/decode limits. One local 24-tile trial finished at 708 ms with four requests versus 811 ms with two, but first detail was delayed from 166 ms to 291 ms. Keep the default unless measurements on your workload justify more queued work.

`RIVER_WEBP_METHOD` optionally selects libwebp encoding effort (0–6) while retaining quality 82 and alpha compression. Unset preserves libwebp's default effort. In a packaged 24-tile corpus trial with encoded caching disabled, method 0 reduced summed encoding time from 303 ms to 66 ms and repeat-view wall time from 453 ms to 276 ms; cold completion was essentially unchanged in that individual comparison. Output grew from 282 KB to 315 KB, with RGB PSNR 38.65 versus 38.52 dB against source pixels and exact alpha. This is an explicit CPU/output-size tradeoff, not lossless output. `tools/verify-native-tile-reuse.py --api URL --format webp` records the packaged encoder's timings, size, aggregate RGB PSNR and alpha check; PNG mode remains exact-pixel verification.

File-backed OpenJPEG native tiles also have a bounded decoded-RGBA cache (`RIVER_NATIVE_TILE_CACHE_BYTES`, default 64 MiB, zero disables). Keys include the asset snapshot, file path/size/modification time, native tile, reduction and quality layers. Matching misses coalesce; active readers keep leases through eviction. Only requested native tiles are decoded. This cache currently does not cover S3 memory-subset ROI decoding. `tools/verify-native-tile-reuse.py --api URL` checks all 24 full-resolution PNG tiles of Bretagne2 against independent pixels; disable encoded caching with `RIVER_RASTER_CACHE_BYTES=0` to isolate native reuse. In one same-image local comparison, native caching reduced two-client full-view completion from 1.83 s to 0.96 s cold and 1.74 s to 0.28 s repeat; OS caches were not flushed and browser painting is excluded.

Native traversal is limited by decoded work as well as tile count: over 8 Mi pixels, or more than 16 times the requested output pixels, uses ROI instead. This avoids constructing giant native rasters for small requests. `tools/verify-huge-native-roi.py` creates a disposable image with 4096-pixel native tile edges and verifies three full-resolution 512-pixel PNG requests use ROI with exact RGBA pixels.

Local image selection also probes bounded, version-pinned file ranges to recover actual dimensions, component precision, resolution levels, quality-layer count and native layout. Concurrent selections share one metadata initialization; ordinary catalog paging does not probe every source. This replaces the catalog loader's provisional seven-level value before manifest or tile delivery, preventing requests for a nonexistent coarsest level. The probe reuses the S3 header parser through a local object-store adapter and does not hash the full source. The real-file verifier above checks sixteen concurrent manifests, the actual six-level coarsest preview, and full-resolution pixels.

WMTS GetCapabilities is paged too: omit `LAYER` for at most 50 registered images by default, use vendor `LIMIT` (1–100) and continue with `CURSOR` from `X-River-Next-Cursor`. It skips locator aliases and never scans S3 provider listings. Only the bounded page's local headers are resolved, with four metadata initializations at once; invalid header entries are omitted. Set `LAYER` to request one image, including an opaque S3 locator for lazy selection. Capabilities advertise the actual resolution levels and omit OpenJPEG resource URLs for detected HTJ2K sources.

`RIVER_S3_NATIVE_TILES=true` experimentally enables OpenJPEG native traversal and decoded-tile reuse for S3 memory subsets. The stream owns the compressed buffer until OpenJPEG releases it; cache keys use the S3 version snapshot rather than the varying subset length. It defaults off: a 24-tile cold WMTS trial on Bretagne2 took 1.65 s with native traversal versus 0.83 s with the default ROI path, despite exact pixels and identical source bytes. Transport tiles crossing native boundaries can make full native decoding more expensive than ROI. `tools/verify-native-tile-reuse.py --api URL --s3` discovers the unchanged source through a temporary paged S3 fixture, verifies every PNG tile, records range bytes/requests, and requires zero source IO on repeat.

On the synthetic 2026-10-14 pixel fixture, PCRL/CPRL previews dropped from 1,881,088 bytes to 344,320/346,880 bytes. They needed 538/537 ranges versus 199 previously. With persistent HTTP connections and 10 ms added range latency, sixteen-way reads completed those previews in 589/570 ms versus the earlier 773/739 ms; four-way fine reads were slower at 1,715/1,683 ms. These are individual local trials, not a universal production speedup. All five progression orders now pass the same preview-transfer gate (under 25% of object bytes), alongside exact preview/detail pixels and cancellation/refinement checks.

`python tools/benchmark-s3-corpus.py --api http://127.0.0.1:18394 --complete --repeat-runs 5 --output reports/corpus.json` measures tiny, small and roughly 1 GB unchanged corpus objects. Add `--range-latency-ms 10` to simulate range-service latency, or `--viewers 16 --distinct` for separate cold object identities. Cold means fresh River source/range/raster identities; the OS cache is not flushed and browser decoding/painting is excluded. On the 2026-10-14 local OpenJPEG trial, reducing resolution-1 read-ahead cut large-image preview-plus-final source transfer from 22,421,730 to 3,126,498 bytes. Overlapping prefix probes reduced preview coverage with 10 ms added per range from 12.27 s to 8.72 s, reading the same 956,642 bytes. These are individual trials, not production guarantees. Sources without TLM still require sequential SOT traversal to discover distant tiles; this remains a cold-latency limitation even when very few payload bytes are needed.

Discovered tile-part offsets also have a disposable disk metadata cache: `RIVER_S3_INDEX_CACHE_BYTES` defaults to 32 MiB (zero disables it), with at most 256 records and 4 MiB per record. `RIVER_S3_INDEX_CACHE_ROOT` defaults to `RIVER_S3_CACHE_ROOT/technical-index`, or the temporary River cache directory. Records are snapshot-keyed, checksummed, and validated for contiguous source extents, tile identities, and cursor bounds. Each selected SOT is checked against the source before use. Missing, corrupt or outdated records fall back to ordinary lazy traversal; writing a cache is never a serving prerequisite. Admission evicts the oldest-written owned records, leaving unrelated files alone. One bounded temporary record may exist during an atomic replacement. Use a private cache directory per River instance; mount it to retain metadata across container replacement. Source reconnection must preserve its source ID and object version to reuse the snapshot record.

Disk-backed tile-part vectors are released between requests by default, including their allocated capacity. `RIVER_S3_RETAINED_INDEX_BYTES` optionally keeps up to 1 MiB per source in RAM (default zero); the next preparation restores offsets from the version-checked disk record. If disk caching is disabled or fails, an at-most-1-MiB per-source fallback preserves incremental discovery. Valid partial progress is saved on cancelled/budget-limited traversals as well as completed ones. Discovery tracks outstanding tile-part counts instead of repeatedly scanning the whole vector for every newly encountered part. These bounds cover the tile-part index; the decoding-header payload cache described below handles main headers, while lightweight catalog descriptors remain resident.

Main and tile-header COM comment payloads are skipped by extent instead of being downloaded and retained in selective decode subsets. Coding markers and essential JP2 color/component metadata remain intact; original objects are unchanged. Marker extents, counts, read budgets and deadlines remain bounded. A synthetic 720,072-byte main-comment fixture required only 13,312 header bytes and retained exact preview/detail pixels; a separate tile-comment fixture is covered by the same pixel verifier. Full-object decoder fallback can still fetch comments with the original bytes. This optimization does not bound the entire loaded-manifest/header registry.

Decoding-header payloads now have a separate shared LRU (`RIVER_S3_HEADER_CACHE_BYTES`, default 32 MiB, zero disables it), capped at 256 entries. Loaded asset descriptors hold only small geometry/coding fields, a header digest, and weak references to the main-header/container/TLM buffers. Eviction releases idle buffers; an active preparation leases its payload until finished. A later request reloads only version-pinned headers, checking the complete descriptor and a digest of coding, container and tile-index metadata before decoding. It does not require re-registration or hashing/downloading the full object. The byte budget counts retained vector capacities; small bounded cache bookkeeping and active leases are additional. Asset manifests, locators and source descriptors still remain resident, so this is not a global bound on all catalog metadata. Exact pixel fixtures cover disabled caching, a 1 KiB cache, restart/reload, and native HT inputs.

An unspecified final tile-part length (`Psot=0`) is resolved against the bounded codestream extent and a ranged EOC check. For JP2 this is the `jp2c` box, so following metadata boxes are not treated as image entropy; raw J2K uses the object extent. Technical lengths use 64-bit arithmetic, including unspecified extents above 4 GiB, while normal read/packet/memory budgets still apply. Synthetic raw and boxed fixtures verify exact preview, detail and final-tile pixels against the same entropy data with an explicit positive length. The boxed fixture includes a trailing metadata box; its reference uses the positive-length counterpart because Pillow's decoder fails on that unspecified-length form. Originals are not rewritten; temporary subsets receive explicit tile-part lengths.

`--restart-container river-goal-TEST` adds a process-cold sample to the corpus benchmark by restarting only that task-owned test container while keeping its S3 fixture alive. Raster and compressed-range memory caches are empty after restart. In the 2026-10-14 10 ms range-latency trial, a 31,585-byte metadata record for 451 source tile parts reduced the large-image restarted-process preview coverage to 5.15 s, compared with 8.23 s for a genuinely new River identity; both read 956,642 original bytes. This improves restart/reload behavior, not first-ever cold traversal. `tools/verify-s3-ranges.py --api URL --restart-container river-goal-TEST` verifies exact preview/detail pixels again after restart (use a test backend with `RIVER_REFINE_IDLE_DELAY_MS=1500` for the preceding cancellation/refinement checks).


Checkpoint 63 combined deployment
-------------------------------

`dist/river-deploy-2026.10.15-combo-m63-final.zip` contains the combined UI/backend image, Compose files, CPU/GPU launch scripts and checksums. Container profiles default to `auto`, sizing decode/IO admission and in-memory caches from CPU affinity, cgroup v2 CPU/memory limits and host memory. Explicit environment settings win. `compact` retains conservative admission and `throughput16` is still available. No CPU quota is imposed by the bundle. WebP defaults to method 0 at quality 82; method 4 can be selected for smaller payloads. Native Grok is enabled with CLI compatibility fallback.

Encoded tiles also have a disposable persistent cache: `RIVER_RASTER_DISK_CACHE_BYTES` defaults to 256 MiB (0 disables it), with at most 4096 records and 8 MiB per tile. The directory is `RIVER_RASTER_DISK_CACHE_ROOT`, or `RIVER_S3_CACHE_ROOT/encoded-tiles-v1`. Admission is serialized and bounded; owned records are atomically replaced and checksummed, including dimensions. Source snapshot, path, driver, format, geometry, quality layers and encoder settings distinguish entries. Corruption falls back to decoding and can be repaired. A hit is promoted to memory. Private cache directories are per instance; originals are never rewritten. Full compatibility-source copies remain process-local.

Alternating old checkpoint 61 / new checkpoint 63 measurements (three runs, one Docker host; fresh S3 identities and compressed caches, OS cache warm) are in `reports/combo-comparison-m63/summary.json`. Median tiny final delivery: 23.6 to 16.4 ms; 5.4 MB image preview/final: 58.2/627.2 to 52.0/516.3 ms; 16-distinct-viewer cold preview p95: 411.8 to 315.4 ms. The 979 MB PNG cold preview was unchanged at about 507 ms. With 10 ms injected S3 latency, WebP preview coverage was 4872 to 4752 ms, a modest 2.5% improvement. Warm results remain around 2–4 ms with mixed sub-millisecond changes. The small-image complete WebP stream grew from 214,717 to 267,690 bytes (25%); speed is a CPU/bandwidth tradeoff. These protocol timings exclude manifest open and browser paint. A separate single restart check changed large PNG preview coverage from 376 to 2.7 ms via persisted tiles; the new process read 2 KiB of metadata during reopen, then reused encoded pixels. Larger prefix read-ahead was rejected because it increased requests from 734 to 931. Larger TLM batches keep the same 734 requests and 750,818 bytes on the large fixture.


Large cold loads and persisted encoding / checkpoint 69
------------------------------------------------------

The deployable combined UI/backend is `dist/river-deploy-2026.10.15-cold-stream-m69.zip`. Its `PERFORMANCE.json` contains the comparison against checkpoint 63; raw trials are in `reports/large-cold-m69`.

The unchanged 979 MB fixture has 451 native tile parts and no TLM table. Ordered SOT/Psot discovery dominated cold preparation, rather than the decoder. Full-image, lowest-resolution previews now fuse a bounded 2 KiB prefix probe into that discovery (`RIVER_S3_COLD_PREFIX_PROBE_BYTES`, 64 through 4096). This is limited to compatible single-layer, index-free sources at least 64 MiB with at most 4096 native tiles. Partial viewports and indexed sources retain their existing planning. Cached prefetch work also bypasses another job's network-fetch mutex.

The viewer requests `progressivePatches:true` on viewport messages. Eligible large full-image previews stream spatial patches while the index walk continues. Patches carry `sourceTileEdge`, `preview:true` and `previewPatchCount`; output dimensions remain display pixels at the requested zoom. They are actual decoded source pixels, not synthesized placeholders. Final tiles and legacy clients retain the original grid; raw tile batches and WMTS are unchanged. Patches are batched within rows to preserve compact ROI rectangles, including supported zero-origin NVIDIA grids. `RIVER_PREVIEW_PATCH_EDGE` accepts 1024, 2048 (default), or 4096 source pixels. The compact preview cache is filled before announcing full preview coverage, so subsequent requests and restarted instances can use the usual small response instead of replaying many patches. This uses derived caches and never modifies original imagery.

Three alternating same-host trials, CPU JP2Grok, fresh S3/encoded/index cache identities and warm OS cache: first non-black PNG data changed from 354 to 67 ms locally, and from 4497 to 765 ms with 10 ms simulated range latency. Complete PNG preview coverage changed from 457 to 401 ms locally and 4670 to 4211 ms with injected latency. Full WebP final delivery changed from 887 to 794 ms locally and 5937 to 5486 ms with injected latency. Warm previews remain approximately 3 ms. These protocol timings exclude manifest opening and browser decode/paint. Independent original-source pixel checks cover every spatial patch. Source traffic changed from 734 requests / 750,818 bytes to 354 requests / 1,050,850 bytes: fewer round trips at the cost of about 40% more compressed preview data, still about 0.11% of this source. Complete coverage still has a serial discovery floor for files without length tables; the larger improvement is time to the first useful image data. OpenJPEG and NVIDIA also pass the large-source patch pixel oracle.

Encoding is now configurable without restarting River: `GET /api/settings/encoding` returns `{"webpMethod":0}`. A `PUT /api/settings/encoding` with `{"webpMethod":4}` persists and applies a method from 0 through 6. The viewer exposes this under **Server encoding**, without a token. Saved settings override the startup `RIVER_WEBP_METHOD` fallback. Storage is `RIVER_ENCODING_SETTINGS_FILE`, or `encoding.json` beside `RIVER_S3_CONNECTIONS_FILE`; the combined bundle mounts that directory in the persistent `river-config` volume. A missing/unwritable persistence target rejects the update without changing the active value. Each render group snapshots the method, and encoded memory/disk cache keys include it, so changing settings cannot return an earlier method's cached WebP. PNG cache entries remain reusable. Validation, method changes, cache separation, CPU/GPU rendering and restart persistence are exercised by `tools/verify-encoding-settings.py`.

A separate 100 ms per-range latency check exposed the prior monolithic preparation deadline: checkpoint 63 returned `S3 range-read deadline exceeded` before any preview. Checkpoint 69 returned exact non-black preview data at 4.68 seconds and finished coverage at 37.6 seconds, reading 1,050,850 bytes in 354 ranges. Bounded patch jobs keep individual preparation work inside its deadline while the full preview streams for longer. This is one supplemental simulated-latency check, separate from the three-run comparison. Cancellation after the first spatial patch also stops pending source reads; the real renderer test verifies source-coordinate placement and that late previews stay below final tiles.


Bounded reuse and fair cold admission / checkpoint 72
----------------------------------------------------

The combined deployable UI/backend is `dist/river-deploy-2026.10.15-reuse-combo-m72.zip`. Matched checkpoint-69 comparisons and raw trials are in `reports/reuse-vs-69-m72`; the bundle includes `PERFORMANCE.json`.

Preparation now retains validated SOT records, parsed tile headers, packet orders and successfully parsed inline packet lengths within the request. The shared budget is `RIVER_S3_PREP_METADATA_BYTES` (default 4 MiB, zero disables admission), with additional entry-count caps. Source SOT validation is still performed before trusting persisted offsets; only repeated validation of the same immutable, version-pinned bytes within that request is skipped.

The technical index has a separate bounded RAM LRU (`RIVER_S3_INDEX_MEMORY_BYTES`, default 32 MiB, zero disables it), at most 4096 records and 1 MiB per record. Entries are keyed by source snapshot and checked against the immutable header digest and object size. Patch jobs reuse it instead of repeatedly reading, checksumming and parsing the disk record. Disk checkpointing is considered after preparation: writes coalesce until at least 128 newly discovered parts or a one-second interval, with forced saving of changed final/error progress. Disk records keep their checksum/extent validation, atomic replacement and existing byte/file limits. This does not retain an unbounded index in every browsed asset descriptor.

Eligible cold previews capture normalized RGBA before encoding, stitch those original decoded pixels, and encode compact cache tiles directly. Already-lossy WebP patches are never decoded/recompressed. This removes the second JPEG 2000 decode previously used to warm compact preview tiles. `RIVER_PREVIEW_PIXEL_REUSE_BYTES` bounds the shared assembly budget (default 32 MiB); per-image capture is capped at 4 million pixels, with conservative bookkeeping reservation. Active batch capture buffers and request metadata are additional bounded work. Encoded caches explicitly discard transient RGBA. Missing captures, unsupported origin/alignment, or insufficient admission fall back to the previous compact decode path. WebP method snapshots and persistent cache separation remain intact.

During serial discovery, River also publishes validated prefixes into that bounded RAM cache. A request whose complete native tile-part set is known can bypass the discovery gate and use those offsets while another request continues scanning later parts. Unknown tile-part totals require a complete index. Missing or evicted metadata falls back to normal discovery. Source misses use a fair cancellable queue, so the scanner cannot repeatedly reacquire the fetch lock ahead of ready payload work. This improves competing requests without adding speculative GETs, increasing I/O limits, or rewriting source files; the first distant tile in an unindexed object still needs its preceding offset chain.

The first patch batch runs before later batches can take its index lock. Large index-free jobs also wait on an asynchronous per-object preparation gate before acquiring a global source-IO permit. Jobs that still need undiscovered offsets retain the gate through blocking discovery and cancellation. This avoids spending global I/O capacity on sibling jobs blocked on the same index. Cached tiles bypass this work. Pure encoding from reused pixels requires CPU admission, not another source-IO slot.

Three alternating matched trials against checkpoint 69, same CPU/native JP2Grok path, WebP method 0, fresh S3/encoded/index/compressed identities, warm OS cache: single large PNG preview coverage 485 to 423 ms (13%); eight distinct large viewers cold preview p95 1648 to 1027 ms (38%), and 7774 to 4469 ms (43%) with 10 ms simulated range latency. Their first-frame p95 changed from 408 to 158 ms locally and 2750 to 598 ms with injected latency. Warm previews remain about 3 ms for one viewer and 5 ms for eight. Single-image complete coverage with 10 ms range latency remains dominated by ordered SOT discovery (4444 to 4407 ms, approximately unchanged). Full local WebP delivery changed from 953 to 888 ms. All compared cases transferred identical original-source byte counts and range-request counts. The 979 MB preview still reads 1,050,850 bytes in 354 ranges. Protocol timing excludes manifest opening/browser paint. PNG pixels match the independent original-source oracle; compact WebP payload hashes match checkpoint 69 byte-for-byte. A separate three-run NVIDIA comparison measured median cold preview coverage 539 to 431 ms, with exact pixels. Small-file timings varied by a few milliseconds across follow-up trials; gains are concentrated in large-image work and concurrent admission. Source images are unchanged.


### Search, image selection, and API switching

The viewer defaults to all connected S3 sources and collects the first 50 matching image keys, then offers Next and Previous pages. Substring search is the default; prefix search remains available. The server endpoint `/api/assets?source=all&q=needle&limit=50` scans provider listings incrementally, never image bodies. Each response performs at most ten provider calls or two seconds of listing work (an in-flight provider call can take longer), returns no more than the requested matching count, and carries a query-bound opaque continuation. The viewer continues these bounded responses sequentially, with a short pause, until 50 matches or exhaustion; changing the query aborts the old search. Large buckets take time for arbitrary substring searches because S3 indexes prefixes, not substrings. Direct `/api/s3/sources/ID/objects` browsing retains one bounded provider page per request.

Settings includes an API URL and Connect button, persisted in this browser. Use this server resets it to the UI origin. Connecting reloads the viewer so sockets and selections cannot remain attached to the previous API. Cross-origin APIs must allow the viewer origin through CORS: set `RIVER_CORS_ORIGINS=https://viewer.example.com,http://localhost:18080` on the target server (comma-separated exact origins; the existing loopback viewer remains the default).

The packaged UI contains no whole-slide/cell overview. Its only bundled demonstration image is Bretagne2.j2k; private tiny synthetic codec warmup probes are not catalog assets. Selecting an unavailable source displays its error instead of substituting unrelated imagery. Asset changes clear old renderer textures even when the zoom is unchanged.

For aligned, zero-origin 1024-square native tiles whose observed minimum range-read latency is at least 5 ms, large progressive previews now use native-row 1024 patches (up to the existing 4096-patch cap), a first batch of at most eight patches, then batches of 32 within each row. Fast or unmeasured sources and other layouts retain the 2048 default. `RIVER_PREVIEW_PATCH_EDGE` overrides the patch size; `RIVER_PREVIEW_PATCH_BATCH_TILES` overrides throughput batch size (1 to 32). Source bytes and final grids remain unchanged.


### Streamer tile URL compatibility

Swagger groups the alternative route under **Compatibility**:

```sh
TILE_URL_TEMPLATE=/streamer/v1/${z}/${x}/${y}/tile.${ext}?image=${imageUrl}
```

In JavaScript, encode the entire inner URL as one query value:

```js
const tileUrl = `/streamer/v1/${z}/${x}/${y}/tile.${ext}?${new URLSearchParams({ image: imageUrl, driver: 'openjpeg' })}`;
```

The object URL itself must use normal URL escaping for its key (`#` as `%23`, literal `%` as `%25`, spaces as `%20`). `URLSearchParams` then encodes that URL for the outer query; do not concatenate a presigned URL directly after `?image=`. Its `&` characters would become separate River query parameters and an unescaped `#` would never reach the server. River resolves HTTP object URLs through connected S3 credentials; their signing query is not forwarded. The UI viewer uses WMTS/WebSocket requests rather than this compatibility template. The separate Nginx UI deployment must proxy `/streamer/` alongside `/api/` and `/wmts`; River errors include `X-River-Request-Id` and a JSON body, whereas an HTML response usually indicates a proxy/static fallback.

URL-encode `imageUrl` when substituting it. It accepts a River asset ID or S3 locator, `s3://bucket/key`, and HTTP object URLs matching exactly one connected S3 source (AWS path/virtual-host URLs and custom endpoints). It opens image headers lazily with existing connection credentials; no separate asset registration or full-object download is required. Unknown endpoints and ambiguous connections return an error. When geometry overrides are omitted, this route uses the existing WMTS decoding/cache path, 512-pixel tile edges, and asset-local matrix numbering: `z=0` is the coarsest resolution and the highest advertised level is full resolution. It is not a geographic slippy-map projection. Supported extensions are `png`, `webp`, `avif`, and `jxl`, subject to runtime encoder availability. Optional `driver` and `layers` query parameters match WMTS.


### Viewer startup independence (checkpoint 81)

Initial image loading and driver discovery do not wait for the S3 catalog to collect its first 50 matches. Catalog search continues independently and retains its paging/cancellation bounds. Pending searches display Searching rather than a definitive zero-image count; completed search failures stay visible. Decoder errors include the actual message in the status panel.

With a connected listing delayed by two seconds, three alternating browser navigation trials measured median first presented image at 2341 ms on checkpoint 79 versus 501 ms on checkpoint 81. This is a UI startup measurement with server-side image caches warm, including browser/automation overhead. The backend binary is identical; this does not change the remaining serial SOT discovery cost for a fully cold large source.


### High-capacity source admission (checkpoint 83)

`RIVER_PROFILE=throughput16` now admits 16 source-preparation jobs instead of eight. Set it in the deployment `.env` for a larger instance. Automatic/compact defaults remain unchanged, and explicit `RIVER_SOURCE_IO_CONCURRENCY` overrides still win (the server caps this setting at 16). Per-job range admission and all byte, cache, cancellation and version-pinning limits are unchanged. This increases overlap between independent cold sources; it cannot remove the ordered SOT dependency within one index-free source.

Three alternating trials with 32 simultaneous distinct cold S3 identities of the same unchanged 979 MB fixture, 10 ms injected range latency, and identical eight-slot CPU settings measured median per-trial p95 preview coverage of 17.68 s at eight source jobs versus 8.84 s at sixteen. First-frame p95 was 208 versus 113 ms. Each transferred 33,627,200 source bytes in 11,328 ranges; peak origin GET concurrency was eight versus sixteen. Cgroup peak memory after three trials was approximately 286 versus 283 MiB, including caches. Warm preview p95 remained around 15 ms with no source IO. These are protocol measurements on the current 22-logical-CPU Docker host, not browser-paint times or a claim about arbitrary 32-image catalogs. A final image validation reproduced 8.83 s p95; a separate single-source PNG check matched the independent original-source reference.

JPIP research: OpenJPEG's [OpenJPIP documentation](https://github.com/uclouvain/openjpeg/wiki/OpenJPIP) and [index manager](https://www.openjpeg.org/doxygen/index__manager_8c.html) describe embedded codestream/tile-part indexes (`cidx`, `tpix`, `faix`). A bounded 33-file local sample read only 1,704 box-header bytes and found none. Therefore, adding an embedded-index reader would not accelerate this sample's first index-free traversal; a separate validated index cache or an existing indexed source remains necessary for that route. Original images were not rewritten, and no new JPIP wire-protocol support is claimed.


### Cached SOT prefix probes (checkpoint 85)

During lazy tile-part discovery, an already cached complete SOT header no longer forces a full prefix read beyond the cached block. Missing packet bytes are still acquired by the following bounded prefix planner. Uncached SOTs retain fused prefix probes. This particularly avoids unnecessary round trips among short adjacent tile parts; version pinning and parser validation are unchanged.

Three alternating matched auto-profile JP2Grok/PNG trials against checkpoint 83 measured the unchanged 979 MB fixture at 354 versus 341 ranges and 1,050,850 versus 1,038,562 source bytes. With 10 ms injected range latency, median first non-black output changed from 54 to 44 ms and complete preview coverage from 4352 to 4209 ms (about 3%). Local coverage was essentially flat (395 versus 399 ms). Repeated previews retained zero source IO. CPU and GPU pixels remained exact, compact WebP cache payloads matched the baseline byte-for-byte, and the 22-fixture range suite plus 64 unit tests passed. GPU native-submit timing showed first-use variability in both versions despite byte-identical prepared inputs, so these timing gains are claimed only for the matched CPU trial. No original images changed.


### GPU resolution-view prefix reuse (checkpoint 87)

The private GPU view builder reuses bytes in its existing initial 64 KiB file buffer instead of issuing overlapping pread calls. Spans outside that buffer retain bounded reads; the before/after file-version guard remains in force. Synthetic fixtures verify independent original-source pixels for both a small file served entirely from the buffer and a larger file requiring further reads, and reject a source changed during extraction. No served image is modified.

Three alternating fresh-container trials of an identical 4652-byte prepared input bound read-only from Windows measured view preparation at 4.70 versus 2.48 ms and total RPC at 13.43 versus 12.27 ms. Reported file-read bytes fell from 33,716 to 4,652, and the encoded RPC payloads were byte-identical. This is a small GPU preparation/resource improvement, not a claim that full cold S3 coverage is materially faster. The unchanged 979 MB S3 fixture still used 341 ranges / 1,038,562 bytes; PNG pixels were exact and repeat source IO remained zero. Additional coarse warmup showed no demonstrated gain and was not shipped.

HTTP error responses use JSON with `code`, `message`, `details`, `status`, `path`, `method`, and `requestId` (also `X-River-Request-Id`). This includes malformed queries/bodies, unsupported methods, and missing resources. Tile errors distinguish an unavailable matrix from `tile_out_of_bounds`, whose details contain the valid row/column bounds. WMTS/compatibility matrix 0 is the coarsest image level; WebSocket z=0 is full resolution. WebSocket errors retain the request sequence and explain validation/decode failures.

Native GPU batches reuse their eight parsed decoder states in synchronized waves. A batch exceeding eight native tiles waits once before each reuse wave, while retaining exclusive state and workspace ownership through completion. Checkpoint 95 measurements and qualifications are in `reports/native-waves-m95.json`; source images remain unchanged.

For small full-resolution/full-quality requests that select every native tile, River prefetches the required codestream payload before probing individual tile headers. Reduced, layer-limited and partial requests retain their existing selective preparation paths. Checkpoint 100 records the request-count, latency, pixel-reference and selectivity evidence in `reports/full-payload-m100.json`.

Large lowest-resolution views reuse optional persisted technical indexes after process restarts. For selected tiles with uncached headers, the reader combines the header and bounded low-resolution prefix probe, then retains those blocks within the existing 8 MiB request-local limit. This reduces dependent S3 round trips even with a small shared cache. It does not require indexes in source images or rewrite them. Checkpoint 104 evidence distinguishes first-ever source access, restart with an index, and hot repeats in `reports/index-prefix-m104/summary.json`.

Source admission distinguishes serial tile-part discovery from independent payload fetches. Large unindexed and anonymous-TLM images enter the per-image discovery gate before taking global I/O slots, including multi-layer images. For those additional image types, the worker releases discovery admission after the shared index phase; existing single-layer prefix preparation and explicit-TLM parallel paths retain their policies. Checkpoint 108 starvation, adjacent-region and capacity evidence is in `reports/source-fairness-m108.json`.

Tile-part index caches permit up to 4096 entries within their existing byte budgets (32 MiB each by default for memory and disk). This prevents an image-count ceiling from evicting useful offsets while most of the byte budget is unused; filesystem scans remain bounded and cache eviction ignores unrelated files. Checkpoint 110 working-set and restart evidence is in `reports/index-capacity-m110.json`. Generated deployment bundles use a stable Compose project name; preserve an existing project name and .env when upgrading older folder-named deployments.

For shared compressed-byte budgets below 8 MiB, each source preparation keeps at most 16 KiB of recently read blocks and uses the existing 8 MiB required-packet lease bound to keep useful prefetched bytes readable after shared-cache eviction. No extra bytes are fetched for this locality buffer, and it is discarded at the end of preparation. Packet-header probes grow by fetching only their missing suffix and reuse the owned prefix during subset assembly. `RIVER_S3_RANGE_CACHE_BYTES=0` now explicitly disables shared block retention (older builds treated zero as the default budget); header, index and rendered caches remain independently configured. Generated bundles optionally forward this setting from .env without changing profile defaults when it is omitted.

Cold S3 opening normally combines metadata selection and the first 1024-byte reader block in one ranged GET, using the full object size returned by Content-Range. The initial bytes and object version come from the same response, and their I/O/latency is included in reader statistics. Clipped objects below 1024 bytes are supported. A missing GET version triggers HEAD compatibility with a pinned reread; ignored ranges and intervening version changes fail explicitly. Evidence is recorded in `reports/header-get-m120/summary.json` and `reports/initial-s3-probe-m120.json`.

Lowest-resolution packet reads remain batched when shared compressed-byte retention is disabled. Dependent packet-header rounds fetch earlier required bodies alongside the next header, reducing separate tiny GETs. Low-memory readers check the existing 8 MiB request-local lease budget before each batch and retain the ordinary reader when the batch cannot fit. Three alternating checkpoint 124/128 comparisons on a 13.65 MB, two-resolution RGB fixture with 10 ms added GET latency measured cold preview coverage at 1330/650 ms with zero shared cache and 800/631 ms with the default cache. Pixels and transferred bytes matched; GET counts fell from 71/68 and 97/65 respectively. This improves packet fetching, not sequential SOT discovery on the unindexed 979 MB fixture. Evidence: `reports/uncached-prefix-m126/performance.json`.

Checkpoint 130 sizes the opt-in `throughput16` native Grok shared pool to eight threads instead of two. Three alternating eight-image trials on a Docker host with 22 logical CPUs and 11.36 GiB RAM measured median cold-preview p95 at 2205/1386 ms and repeat-preview p95 at 1655/937 ms. Pixels and source traffic matched. Peak cgroup memory rose by about 82 MiB in this workload; this is a throughput tradeoff for larger instances. Compact/auto defaults and explicit worker overrides are preserved. Evidence: `reports/prefix-capacity-m129/performance.json`.

S3 sources share HTTP transport clients within the same HTTP-permitted or HTTPS-only policy. Signing credentials and session tokens remain per source; connection pools remain per origin. SDK-internal clients requesting different options retain their own transport settings. Eight distinct buckets/credentials on one HTTP/1.1 origin used eight versus one TCP connections in checkpoint 130/138. With a synthetic 50 ms cost per new connection, median opening time for subsequent cold image identities changed from 59.1 to 4.2 ms; opening all eight sources changed from 498 to 113 ms. The first source still took about 60 ms, and both versions read 8192 bytes in eight GETs. This is connection-setup evidence, not a real TLS measurement. Pixels, signing credentials and session tokens were checked independently. Evidence: `reports/s3-transport-m138/performance.json`.

Complete-source compatibility copies use four ordered 1 MiB transfer lanes by default. Set the container environment variable `RIVER_S3_COMPATIBILITY_CONCURRENCY` to 1–8 to tune this bounded payload buffering. The pipeline reuses version-pinned, size-checked range reads with cancellation, a 10-second individual range timeout and a 300-second overall copy deadline. Failed and cancelled partial files are removed; only completed files enter the compatibility cache. A deliberately forced fallback (64 KiB selective-read budget) improved median preview completion on a 13.65 MB fixture from 1573 to 1142 ms over three alternating trials at 25 ms added GET latency. A single 979 MB original-image pilot at 10 ms added latency changed from 55.6 to 33.7 seconds, with identical bytes, GET count and exact preview pixels. These results apply only to complete-file compatibility copies; normal selective requests and sequential SOT discovery are unchanged. Evidence: `reports/compatibility-pipeline-m140/performance.json` and `reports/compatibility-faults-m140.json`.

### Edge tile padding

Compatibility tiles default to opaque black padding, with the image aligned at the top left of the configured bitmap (512 pixels without geometry overrides). Use `&edge=crop` to return natural edge dimensions or `&edge=transparent` for transparent padding. Regular WMTS keeps cropped edges by default and accepts `&EDGE=black`, `&EDGE=transparent`, or `&EDGE=crop` (lowercase `edge` is also accepted). WebSocket clients set the connection URL, for example `/ws/tiles?edge=transparent` or `/ws?edge=black`; the option applies to viewport and tile-batch messages and every progressive pass. Preview bitmaps retain their nominal reduced tile size and report their padded display extent; final bitmaps use the regular tile size. Padding adds no source-image pixels or reads. Partial edge payloads remain separate for each mode. Full interior tiles on integral, zero-origin grids share their encoded payload across modes when the actual decoded dimensions confirm that no padding was added; mixed batches and prepared GPU offsets retain their original tile identities. `none` is an alias for `crop`. Invalid modes return a structured `invalid_edge_mode` error before rendering.


### Crystal compatibility tile geometry

`/streamer/v1/{z}/{x}/{y}/tile.{ext}` accepts an independent client pyramid by supplying **both** `tileSize` (1..4096 source pixels) and `maxZoom` (0..30). Optional `minZoom` defaults to 0; `tilePixelRatio` defaults to 1. Incomplete geometry overrides return `invalid_tile_geometry` rather than guessing. Without these parameters, existing River matrix behavior is preserved.

```js
const query = new URLSearchParams({
  image: imageUrl, tileSize: '256', maxZoom: '7', minZoom: '0',
  tilePixelRatio: '2', edge: 'transparent', driver: 'openjpeg'
});
const tileUrl = `/streamer/v1/${z}/${x}/${y}/tile.webp?${query}`;
```

The source footprint is `tileSize * 2 ** (maxZoom - z)` pixels per axis, starting at `(x * footprint, y * footprint)` with rows increasing downward. The bitmap edge is `round(tileSize * tilePixelRatio)` (limited to 1..4096). A pixel ratio of 2 doubles bitmap density while preserving coverage. Native JPEG 2000 levels only optimize decoding: River uses an available level without undersampling, then bilinear resampling where needed, including pyramids beyond the native reduction count. Black and transparent padding follow resampling; crop returns only the clipped image area, rounded up to output pixels. Completely out-of-bounds tiles return a detailed 404.

`image` accepts connected-source S3/HTTP object URLs, existing locators and asset IDs; no manifest or prior registration request is required. `URLSearchParams` encodes the complete image URL once as an outer query value, retaining any inner query delimiters. Existing `driver`, `layers`, formats, scheduling, source version pinning and error responses apply. WMTS and progressive WebSocket public geometry remains unchanged.


### Stable and LTS release

Checkpoint **178**, `river:2026.10.19-crystal-geometry-combo-m178`, is the stable/LTS baseline as of 2026-10-19. Local `river:stable` and `river:lts` aliases reference that same verified image; portable deployments keep the immutable dated tag. Release identity, bundle checksum, validation and limitations are recorded in `deploy/releases.json`. Checkpoint 140 remains pinned as the previous LTS rollback. Promotion does not change or rebuild the published 178 bundle.

Validation includes 97 unit tests, 234 geometry cases across OpenJPEG/JP2Grok/nvJPEG2000, legacy compatibility and WMTS/WebSocket regressions, and configuration/cache upgrade persistence. Long-duration production soak and organizational S3 validation are still outstanding. Large unindexed random cold loads remain a known performance limitation. No fixed LTS support end date has been defined.


### Parallel discovery for large unindexed sources

Large sources without TLM offsets can use speculative parallel SOT index walks. Four bounded probe windows seed independent walks, and frontier header reads are batched. A speculative chain becomes usable only when the original main-header walk reaches its exact starting offset via validated `Psot` lengths. Tile IDs, apparent marker signatures and proximity never establish connectivity; false seeds inside comments or packet payloads remain unreachable and are discarded. The persisted technical index remains a normal contiguous prefix, with unchanged version pinning, cancellation and byte/request/deadline budgets.

The path activates for objects at least 64 MiB with at least 128 native tiles, measured range latency of at least 2 ms, and a sufficiently late requested native tile. Whole-view requests spread seeds across the object; partial requests use spatial order solely to place probes. Unordered sources retain the sequential fallback. A timed 64 KiB pilot estimates whether bulk transfer would cost more than the likely RTT saving; slow probes time out after 500 ms and fall back to the sequential walk. Sources get at most one probe attempt per opened snapshot (including cancellation); bounded unconnected lane metadata is retained across progressive patches (at most 8 lanes with 128 records each). First patches and already indexed requests remain demand driven.

`RIVER_S3_INDEX_SHARDS=0` disables this bandwidth-for-latency tradeoff; qualifying large whole-image walks automatically use 4–32 chains (4 otherwise), with a maximum of 32. `RIVER_S3_INDEX_PROBE_BYTES` caps probe traffic (default/max 16 MiB, also limited to a quarter of the remaining compressed-read budget); each window is at most 4 MiB, fetched as bounded ranges of at most 1 MiB. Probe reads consume the ordinary request budget. No source image is rewritten. Fast low-latency, small and TLM-indexed sources keep their existing path. Slow bandwidth-limited deployments can disable or reduce the probe budget. This optimization targets cold index discovery; it adds no new decoder or lossy conversion.


Checkpoint 184 (`river:2026.10.19-adaptive-parallel-index-combo-m184`) was compared against stable 178 on the unchanged 978,812,130-byte unindexed source. Three serial alternating Linux-image trials with 10 ms added GET delay gave median cold PNG tile latency **2779.8 → 1264.8 ms** and full progressive-preview coverage **3984.2 → 1663.1 ms**. OpenJPEG decode time remained about 159 ms; source preparation fell from 2616.1 to 1086.9 ms. Repeated WS coverage was 2.66 → 2.71 ms with zero source reads. One 50 ms GET-delay trial reduced cold tile latency from 11814.9 to 3591.7 ms.

This is a bounded bandwidth tradeoff: tile traffic increased from 3.15 to 19.86 MB and preview traffic from 1.04 to 17.73 MB. The low-latency trials bypassed probing. A single 2 MiB/s origin test skipped bulk probes, reading 3.22 versus 3.15 MB and taking 4.46 versus 4.37 seconds. Exact-pixel tests covered real originals, fake markers embedded in comments, reversed tile order, and all three drivers. These are fresh River-cache identities with warm OS caches, not organizational S3 or browser-paint measurements. Evidence and limits are in `reports/index-shards-m184/performance.json`; 178 remains stable/LTS.


### Actual TiTiler comparison and native reader gap

The installed official TiTiler 2.4.0 image (digest `988cc5e4a600593f241ffbf75c5d7316aecfba873410fa881d916afeb305f81f`) contains Rasterio 1.5.1 / GDAL 3.12.4 with JP2OpenJPEG; Kakadu is absent. Real `/cog/bbox` and `/cog/preview` requests were tested against River 184 on the same read-only 978,812,130-byte JP2, using fresh URL identities and the recommended GDAL range/cache settings. Processes and OS caches were warm; no added GET delay models a nearby storage service, not an organizational S3 measurement.

At zero injected delay, three alternating cold 512x512 tile trials gave medians of 1025.7 ms for TiTiler/OpenJPEG and 402.6 ms for River. A full 641x172 preview pilot fetched 983.76 MB in 18.01 seconds with TiTiler/OpenJPEG, versus 1.04 MB in 366.9 ms with River. Pixel differences were at most one 8-bit level from TiTiler rescale rounding; River matched its independent reference exactly. At 10 ms GET delay, cold tile medians were 3121.8 versus 1258.2 ms. Full reports, exact image digest, drivers, ranges and settings are under `reports/titiler-study/`.

This does not benchmark the proprietary JP2KAK driver or the reported 11 GB image. GDAL documents that Kakadu can exploit TLM/PLT for selective packet reads, while GDAL's OpenJPEG reader ingests participating tile parts in full. The important River gap is general decoder-driven ranged input: its drivers currently share subset preparation, and incompatible layouts can fall back to a whole-object disk copy. The native Grok callback experiment did not solve this gap: the tested detail request asked for almost the complete 979 MB source after its initial header read. Kakadu's GDAL integration applies resolution, region and component restrictions within the compressed-data engine, while its official release history describes background reading/parsing and tile opening/closing. River still has a whole-view preparation barrier before native decoding; the next architectural step is bounded overlap of preparation and decoding, with real wire IO and independent pixels as acceptance criteria.

### Compatibility preparation preflight (checkpoint 191)

River now switches to disk-backed compatibility before reading entropy payloads when tile headers prove that complete payloads exceed the subset memory ceiling. PLT and supported prefix layouts retain selective reads. Compatibility copies reuse version-pinned cached chunk edges without increasing GET count for fragmented caches.

Three alternating fresh-source JP2Grok/PNG trials of the unchanged 82,728,696-byte unindexed RESET-style fixture measured cold preview medians of **7.78 s (178), 6.65 s (184), and 1.95 s (191)** without added GET delay. With 10 ms added delay the medians were **15.72 s, 10.05 s, and 2.98 s**. Traffic fell from 149.81 MB / 803 GETs on 178 to 83.06 MB / 408 GETs on 191. Repeated previews used zero source IO. Full-resolution fixture and detail ROI pixels were independently verified against encoder input; the reduced preview reference uses the same Grok codec on the original file.

This targets large compatibility fallbacks, not every JP2 layout. The unchanged 978 MB selective fixture still matched independent pixels at 1.21 s with 10 ms GET delay. The release uses the verified 184 index planner, excluding the separate unverified placement experiment. Evidence: `reports/compatibility-preflight-m191/`. Stable/LTS remains 178.

### RESET-style selective previews (checkpoint 192)

Classic JPEG 2000 RESET-only codeblocks now use the existing single-segment prefix reader for supported LRCP/RLCP/RPCL layouts. Other classic flags and HT handling are unchanged. Original compressed bytes are preserved; River requests selected packets and reconstructs omitted packets as before.

Three alternating fresh-source JP2Grok PNG trials on the unchanged 82.73 MB RESET fixture measured cold preview medians of **1949.5 -> 535.5 ms** against 191 without added latency, and **2957.4 -> 1401.9 ms** with 10 ms GET delay. Traffic fell from 83.06 to 17.70 MB. Repeated completed previews used zero source IO. Independent GDAL/JP2OpenJPEG overview pixels matched exactly, as did twelve CPU checks across six progression/codeblock fixtures. Progressive coverage was independently exact at 565 ms cold and 2.8 ms repeated; cancelling after the first patch stopped further source reads.

The RESET gain applies to OpenJPEG and Grok. nvJPEG2000 rejected these RESET fixtures with parse status 4 in both 191 and 192; its existing normal-layout large-tile pixel check passed. No new GPU support is claimed. This release retains the verified 184 index planner. Reports: `reports/reset-prefix-m192/`. Stable/LTS remains 178.

### Segment-aware classic prefixes (checkpoint 193)

Selective first-layer prefix reads now understand classic BYPASS and TERMALL segment lengths as well as the context-only coding flags and their combinations. TERMALL takes priority when both segmentation flags are present. The existing progression, layer and geometry restrictions remain; HT handling is unchanged. Invalid or truncated segment-length headers fail explicitly.

Three alternating fresh-source trials of an unchanged 81.09 MB unindexed BYPASS source compared 192 with 193: median cold PNG preview **1904.9 -> 544.2 ms** without injected GET delay and **2961.7 -> 1416.8 ms** with 10 ms delay. Source traffic fell from 81.42 to 17.69 MB. Completed repeats read zero source bytes. Twelve CPU checks across six coding styles, finer-resolution combined-flag/codeblock cases, independent original/reduced pixels, progressive delivery and the range/cancellation suite passed. There are 101 passing release-snapshot unit tests.

This extends OpenJPEG/Grok selective-read coverage; it is not a universal cold-load guarantee or a claim of new GPU coding-style support. Evidence: `reports/classic-prefix-m193/`. Stable/LTS remains 178; release builds continue using the verified 184 index planner.

### Demand-calibrated index probes (checkpoint 196)

The speculative index decision now waits for two required native tile parts and at least one preparation GET. Connection setup on the first header request cannot by itself trigger bulk probing. Nearby objects retain demand-driven SOT reads; higher-latency sources keep the bounded parallel probe path. Cache hits and persisted indexes do not count as a fresh latency sample. Existing cancellation, version pinning, read budgets and bandwidth guards remain.

Three alternating direct calls to never-opened connected S3 object URLs (metadata and tile response included, no registration or manifest request) measured median preview **552.7 -> 387.8 ms** against 193 on the unchanged 81.09 MB BYPASS fixture. Range traffic was about 17.69 -> 1.23 MB. Separately, eight concurrent distinct cold sources measured median per-trial p95 full progressive coverage **2351.8 -> 955.6 ms**, with median total traffic **92.24 -> 10.26 MB**. Both versions repeated with zero source IO. Pixels were independently verified.

The tradeoff remains measured: with 10 ms GET delay, the single-preview median was 1396.0 -> 1415.8 ms (about 1.4% overhead), retaining the parallel-read speedup. The 978 MB random tile was 1220.3 -> 1211.9 ms with the same delay. The 2 MiB/s guard test avoided bulk windows, read 3.22 MB and matched independent pixels. These are warm-process/OS-cache fixtures, not organizational storage or browser-paint claims. Evidence: `reports/demand-calibration-m196/`. Stable/LTS remains 178.

### Partial-view probe staging (checkpoint 199)

Unindexed partial-image requests place seed windows using clipped native-tile area, including partial edge rows/columns and nonzero origins. They initially fetch three quarters of each bounded seed window and fetch its remaining tail only if no candidate seed is found. A retained boundary block covers split SOT markers even with the shared range cache disabled. Full-image requests retain full seed windows. Every speculative chain still requires an exact connection to the original Psot walk; probe bytes never establish trust by themselves.

Three alternating JP2Grok PNG trials against 196 on the unchanged 978.81 MB source measured median cold tile response **1216.8 -> 1060.6 ms** with 10 ms GET delay and **3551.5 -> 3149.2 ms** with 50 ms delay. Source traffic fell from **19.85 to 15.68 MB** with unchanged GET count. Metadata opening is separately recorded. Full-preview traffic/request counts remained unchanged and timing stayed within trial variation. Independent pixels, 103 frozen-release unit tests, progressive cancellation, zero shared range-cache/restart reuse, and the slow-link guard passed. Source images remain unchanged. Evidence: `reports/partial-probes-m199/`.

Docker builds now refresh Rust source timestamps inside the build step before Cargo freshness checks. This prevents restored older snapshots from falsely reusing a shared target artifact while preserving dependency and Docker caches. The release includes the verified probe-placement/staging implementation rather than the previously held unverified experiment.

A separate colocated Linux-origin study measured 196 at 143 ms for a direct 81 MB preview and 157 ms for the specified tile from the 978.81 MB source, including metadata and exact independent pixels. These used byte-identical fixture copies on native Linux scratch, avoiding the Windows/Docker origin path. This is a deployment/benchmark-topology result, not a new application gain or reproduction of the reported 11 GB Kakadu workload. Evidence: `reports/colocated-origin-m197/`. Stable/LTS remains 178.

Checkpoint 210: three alternating cold trials of the 13.69 GB synthetic, unindexed 6,000-tile thumbnail improved from 1,294 ms (research 209) to 946 ms with the same two-thread Grok pool and native Linux HTTP origin. GETs fell from 9,580 to 4,645 while bytes increased from 27.07 MB to 30.86 MB. Independently resampled pixels differ by at most one 8-bit level. Eight native threads measured 762 ms in a separate comparison. Processes and OS caches were warm; every source identity was fresh and metadata opening was included. These results do not establish performance on all multi-gigabyte images or on the organizational Kakadu workload. Details and validation are in `reports/fused-navigation-m210/performance.json`. LTS remains checkpoint 178.

Checkpoint 212 handles SOP/EPH packet markers in selective PLT reads and supported inline prefixes. Kept packet bytes remain intact; omitted packets receive valid empty headers and markers, including wrapping SOP sequence numbers. Optional absent SOP markers are accepted; missing EPH or malformed SOP lengths produce explicit errors. Marked coarsest RPCL views can also use entropy-preserving packing, including legal precincts smaller than nominal codeblocks. Marked empty-packet expansion is checked before allocating its output.

On a derived marker-bearing variant of the 979 MB Sentinel fixture (same original entropy, no tile repetition or image recompression), three alternating direct-cold trials improved preview latency from about 1.07 s to 131 ms and full-thumbnail latency from 929 ms to 101 ms. Preview IO fell from 979 MB to 777 KB; independent pixels were exact, with existing resize tolerance for the thumbnail. These are nearby native-Linux fixture results, not a measurement of the organizational 11 GB image. Exact combo-image results are in `reports/marked-original-study/combo-benchmark.json`; LTS stays 178.


### OpenHT hybrid CPU driver

Checkpoint 213 adds `driver=openhtj2k` to the compatibility endpoint, `DRIVER=openhtj2k` to WMTS, and `driver: "openhtj2k"` to WebSocket requests. The viewer exposes **OpenHTJ2K + Grok fallback**. Existing driver defaults remain unchanged.

The portable isolated worker accelerates conservatively supported coarsest-resolution views of ordinary Part1 JPEG2000. It preserves original entropy bytes; it does not convert source images to HTJ2K. Other layouts, HT-coded input, progressive quality layers, and small packed jobs use Grok. `x-river-decoder-route` and WebSocket timing identify `openht-packed-ranges`, `openht-fallback-s3-packet-ranges`, `openht-fallback-s3-tile-part-ranges`, or `openht-fallback-s3-disk-compatibility`. Local fallback reports `openht-fallback-jp2grok`. Worker failures remain explicit errors rather than silently retrying an expensive decoder.

One exclusive worker is the default. Native thread sizing is automatic: half the available CPU budget divided across workers, capped at sixteen with direct blocks enabled (eight otherwise). Since checkpoint221, negotiated direct frame budgets use at most eight while another source preparation or worker request is pending; explicit numeric settings disable adaptation. Compact retains two; an explicit numeric thread count overrides automatic sizing. Configure `RIVER_OPENHT_WORKERS` (1–8), `RIVER_OPENHT_WORKER_THREADS` (`auto` or 1–16), `RIVER_OPENHT_WORKER_MEMORY_MIB` (256–4096, default 1536 per worker address-space ceiling), and `RIVER_OPENHT_TIMEOUT_MS` (50–60000, default 30000). The tested dense fixture used about 324 MiB worker RSS. Additional workers require measured memory/capacity headroom; two workers doubled memory without a meaningful throughput gain in the released four-source test on the development host. Decoder workers use the bundled mimalloc allocator; `RIVER_OPENHT_ALLOCATOR_LIBRARY` overrides its path, and an empty value disables that override. `RIVER_OPENHT_MIN_PACKETS=0` forces eligible small packed views for diagnostics; the default 16384 avoids their worker startup overhead.

Large full-image index walks on nearby storage now size their speculative chain count from the mean native tile-part size, bounded to 4–32 chains within the existing 16 MiB probe ceiling. The bandwidth pilot and concurrent-source guard remain active. Only chains joined by exact validated Psot offsets enter the index. Leave `RIVER_S3_INDEX_SHARDS` unset/empty for this automatic policy; an explicit integer overrides it, including 0 to disable probes.

The worker is built from pinned OpenHTJ2K commit `dacb452ecb21f50e88a74bf860c60f4aafa8a602` with baseline x86-64 flags. The included patch widens cached precinct indices to 32 bits to prevent reused dense frames from reading truncated indices. Original sources, technical caches, and persisted S3/encoding settings are separate. See `reports/openht-worker-m213/` for benchmark scope and validation evidence; synthetic multi-gigabyte fixtures are explicitly identified and do not establish universal 400 ms cold-load performance.


### Parallel range task scheduling

Checkpoint 214 lets independent S3 range requests perform SDK signing and response work on Tokio worker threads instead of polling all work from one blocking preparation thread. Each batch keeps its existing byte/request limits and a maximum 16 tasks. Dropping a batch aborts outstanding tasks; every task retains the source version, deadline, cancellation receiver, and parent trace span.

`RIVER_S3_RANGE_TASKS=auto` is the default. It enables parallel tasks only for native grids above 1024 tiles, at least four ranges in a batch, at most 16 MiB planned bytes, measured range latency below 2 ms, at most one active preparation, and at least four runtime worker threads. Other reads keep the prior scheduling policy. `inline` disables it; `spawn` forces task scheduling for diagnostics. `RIVER_S3_RANGE_CONCURRENCY` remains the explicit 1–16 concurrency override. No extra ranges or source bytes are introduced by scheduling alone.


### Bounded index chunks and displaced tile headers (checkpoint 216)

Large whole-image S3 walks on nearby storage can advance several successive tile-part headers per runtime task. `RIVER_S3_INDEX_LANE_DEPTH` is automatic when unset/empty: qualifying grids above 1024 native tiles with measured range latency below 2 ms use up to 32 steps, further capped using measured latency and bounded planned lookahead. Other requests retain one-step waves. An explicit 1 disables deeper chunks; explicit values are clamped to 1–32. Whole-image selection, sufficient shared byte-cache capacity and the concurrent-source guard remain required. Exact Psot connectivity remains the only gate for admitting speculative offsets; cancellation and an early failed range abort and drain sibling tasks.

Header discovery uses bounded 256-part batches for large previews instead of switching every tile to serial reads above 4096 parts. Long tile comments can move SOD beyond the ordinary SOT probe: eligible coarsest requests apply the existing small LL probe at the comment's end. This deliberately trades bounded extra bytes for fewer dependent requests. A bounded header-read allowance is reserved before packet eligibility is known; explicit server range caps, byte budgets and deadlines still win. Parsed headers are retained within the existing request metadata budget and reused only for the identical validated tile-part identity, including after compressed-byte eviction.

Qualification and performance evidence: `reports/streaming-index-m216/`. The large 4.37 GB/13.69 GB fixtures are synthetic mapped sources; the 978 MB Sentinel fixture is an unmodified source. Restarted River processes, fresh object identities, structurally warm fresh objects and encoded repeat hits are separate measurements. The source/origin OS page caches are warm; browser painting and organizational-S3 performance are not measured here. Checkpoint 178 remains the LTS pin.


### Direct classic codeblock decoding (checkpoint 218)

Select `driver=openhtj2k` (the OpenHTJ2K hybrid UI option) to use this backend. The ordinary default driver remains OpenJPEG. Eligible zero-DWT, single-layer, no-MCT packed LL views can decode classic Part1 codeblocks directly with bounded per-thread scratch storage, avoiding the full precinct/codeblock object hierarchy. Original source entropy and files are unchanged. Signed and unsigned samples up to 16 bits and grayscale/RGB/alpha normalization retain the existing worker contract. Valid modes outside this path, including zero-byte codeblock segments, use the prior OpenHT decoder; malformed input returns an error.

Checkpoint218 retained the existing decoder for larger-block layouts. Checkpoint221 removes that heuristic after parallel output conversion reduced their direct decoding cost. Component-aligned groups include empty packets and write their own RGBA pixels, avoiding full intermediate sample planes. `RIVER_OPENHT_DIRECT_BLOCKS=true` forces a direct attempt; `false` disables it. Automatic worker selection keeps the existing 16,384-packet rule and also admits directly compatible rasters of at least 65,536 pixels. An explicit `RIVER_OPENHT_MIN_PACKETS` value overrides that automatic policy; leave it empty for automatic selection. Small packed rasters retain their prior in-process route. `RIVER_OPENHT_WORKER_THREADS=auto` and worker-count/memory/deadline controls remain available.

Evidence is recorded in `reports/direct-block-m218/`: independent pixel comparisons, signed/unsigned 8/10/12/16-bit fixtures, mixed direct/legacy frames, malformed-frame failures, source-wire accounting and image-level benchmarks. The large mapped fixtures are synthetic. This does not establish universal 400 ms performance or a licensed Kakadu comparison. Entropy-heavy images and cold navigation to distant unindexed native tiles remain optimization targets. Checkpoint 178 remains LTS.


### Cold distant unindexed tiles (checkpoint 219)

Qualifying nearby-storage partial views can advance bounded index chunks as whole-image previews do. The byte-cache, latency, concurrent-source, depth, read-budget and deadline guards remain active. Only exact Psot-connected offsets enter the main index. Speculative walks pause at any requested missing tile part, including a wanted part already retained in a lane, so later navigation can reuse discovered metadata without extending that walk unnecessarily.

`RIVER_S3_INDEX_PARTIAL_LANES=false` restores prior partial-view scheduling. The default is automatic; `RIVER_S3_INDEX_LANE_DEPTH=1` disables deeper chunks. This is shared source preparation and benefits OpenJPEG and the OpenHT hybrid. It changes no image data and adds no required registration or preprocessing. Evidence in `reports/distant-roi-m219/` includes original-file ROI oracles, actual origin byte/request counts, cancellation, injected version failures and deceptive COM-marker validation.

The many-tile 13.69 GB fixture is synthetic; the 978 MB source is an unmodified Sentinel image. Cold-process/object tests retain warm origin/OS page caches. Later uncached navigation outside previously probed coverage and skipped probes on the bandwidth pilot remain optimization targets. Cached repeats continue to use encoded responses with no origin reads. Checkpoint 178 remains LTS.


### Known-cursor index continuation (checkpoint 220)

A qualifying large unindexed walk can continue in bounded async chunks after its probe decision has been made, even when no speculative lanes remain. The primary cursor follows the verified Psot chain and pauses at requested missing tile parts. This avoids crossing back to the calling thread for every necessary SOT header; it adds no bulk probes. TLM-driven paths retain their existing batching. Read/request/deadline, byte-cache, low-latency and concurrent-source guards remain active.

`RIVER_S3_INDEX_PRIMARY_CHUNKS=false` restores previous handling; the default is automatic. Ordinary native grids skip this policy entirely, environment eligibility is checked once per request, and busy/cache-limited contexts return to their existing path before tuning lookups. The normal initial probe decision is retained, and `RIVER_S3_INDEX_LANE_DEPTH=1` disables deeper chunks. The benefit applies to source preparation for both OpenJPEG and the OpenHT hybrid. Evidence in `reports/primary-index-m220/` separates initial cold-object loads, an uncached move within the same object, and encoded repeat hits; includes independent original-file ROI pixels, actual source IO, cancellation, version failure/retry and false-marker validation. The large mapped fixture is synthetic and origin/OS page caches are warm. Checkpoint 178 remains LTS.

Checkpoint221 applies direct normalized output, bounded scratch and adaptive CPU budgets. Optional frame-budget support is negotiated before sending extended requests; custom older workers keep protocol1 frames. Legacy fallback retains its launch thread budget. Preparation and native decoding are still separate stages; overlapping them is a remaining research target. Same-file organizational Kakadu parity has not been established. Research: `reports/direct-output-m221/kakadu-research.json`.

Checkpoint222 overlaps bounded native-tile decoding with source indexing for supported whole coarsest views. Required cached prefixes enter a two-batch queue in groups of128 native tiles; decoded samples are placed at their original coordinates. Unresolved selected prefixes retain normal version-pinned reads and existing byte/request limits. Originals are unchanged. Automatic selection requires a complete aligned native grid with at least1024 tiles, at least64 samples per coarsest precinct, a bounded output and enough byte-cache capacity. Dense tiny-block layouts, clipped grids and partial selections retain their prior routes. Explicit `RIVER_OPENHT_MIN_PACKETS` overrides and `RIVER_OPENHT_DIRECT_BLOCKS=false` disable this automatic pipeline. `RIVER_OPENHT_PIPELINE=false` is its direct rollback.

Three alternating fresh-process trials against221 measured the synthetic13.69GB whole preview at472.4 ->431.1ms median, with identical30,849,831 source bytes and4655 GETs. Repeated responses remained around2.4ms with zero source reads. Dense/small and unchanged978MB cases remained approximately unchanged. Four-source batches showed no demonstrated capacity gain. These use warm origin/OS caches and do not establish same-file organizational Kakadu parity. Evidence: `reports/overlap-m222/performance.json`.

The new route is `openht-pipeline-ranges`. Its `decodeMs` includes native work performed during preparation; `sourceCacheMs` includes the overlapping preparation wall time. Do not sum those timings. The server wall/response time is the latency metric. Normal WebSocket viewport groups still use their prior fallback; their pixels and cancellation are verified, but no WebSocket speedup is claimed. Extending acceleration to partial selections remains active work.


### Checkpoint224: selected and indexed coarsest previews

Checkpoint224 extends222's whole-view pipeline to supported partial and sparse selections, retaining output origins for correct crops and mixed-cache batches. Two queued batches default to256 native tiles each (`RIVER_OPENHT_PIPELINE_BATCH_TILES=32..512`). Nearby large-grid index depth defaults to128 for partials and32 for whole views; explicit `RIVER_S3_INDEX_LANE_DEPTH` accepts1..128. `RIVER_S3_INDEX_BATCH_SELECTED=false` restores earlier primary requested-tile stops. Dense tiny-block and clipped-grid layouts retain conservative routes.

TLM-selected single-layer coarsest requests fuse bounded header/prefix reads. Three alternating cold River/source trials on the derived13.69GB fixture measured117 ->55ms,1448 ->683GETs, and1.56 ->2.16MB, with exact pixels. With5ms source delay,224 measured298ms indexed versus5.71s unindexed. OS/origin caches were warm; these results do not establish arbitrary organizational-S3 or Kakadu parity. Unindexed request amplification remains active work. Original image files stay unchanged. Evidence: `reports/indexed-prefix-m224/`.


### Runtime configuration and adaptive source reads

`GET /api/settings/configuration` exports one schema-versioned JSON document containing all API-managed S3 connections (including credentials), encoding settings and read policy. `PUT` the same document to that URL to replace them atomically. Validation or persistence failure changes nothing; source IDs must be unique. A successful replacement invalidates loaded remote registrations and clears learned observations. Deployment environment settings such as bind ports, CPU pools and memory ceilings remain startup-only. No administrator privileges or token are required for any River API.

`GET /api/settings/read-policy` returns settings, per-source successful range-transfer counts, header latency EWMA, effective throughput EWMA and the latest read-planning decision. These counts are not physical retry-attempt totals. Measurements are process-local, shared across images from the same configured connection, and reset on restart or connection replacement. No synthetic network probes are sent. SDK reads measure response-header latency separately. Unknown native bulk header latency is omitted while its total transfer time still trains throughput. The `learning` object reports sample thresholds and eligible range kinds.

`PUT /api/settings/read-policy` accepts `{"mode":"adaptive","minimumSamples":16,"maxOverfetchRatio":2}`. Modes are `disabled`, `observe` (default) and `adaptive`. `RIVER_S3_ADAPTIVE_READS` sets the startup fallback; persisted settings win. Observe mode computes and reports decisions while retaining the previous range plan. Adaptive mode estimates parallel GET-wave cost plus transfer cost using the actual scheduling and per-source concurrency limits, and accepts a plan only for a predicted improvement of at least10 percent. At least three32KiB-or-larger successful reads and minimumSamples successful reads are required; insufficientSamples and insufficientThroughputSamples distinguish those requirements. Existing explicit request, total-byte, deadline, cancellation and overfetch limits still apply. Ordinary packet batches retain a1MiB merged-range limit; learned multipart prefixes may use ranges up to8MiB, exposed as learning.maximumMergedPrefixRangeBytes. Their total-byte allowance is unchanged. This applies to eligible packet batches and multipart resolution-prefix batches up to16384 ranges. Fewer GETs can require more bytes, and a fresh connection may have insufficient samples. It does not eliminate absent-index discovery requirements.

All managed settings persist in `RIVER_CONFIGURATION_FILE`, or `configuration.json` beside `RIVER_S3_CONNECTIONS_FILE` (or the legacy encoding settings file). Existing S3/encoding JSON is read on first migration; subsequent changes write the unified document. The viewer exposes JSON export/import and read-policy observations under Server configuration.

An explicitly set `RIVER_S3_RANGE_OVERFETCH_RATIO` caps the API policy. Without that environment variable, the legacy plan retains its2x default, while adaptive candidates may use the API-configured allowance up to4x. GET read-policy reports the explicit cap and effective adaptive allowance.

### Performance comparison and release channels

Use the fixed-suite protocol in [deploy/benchmark-policy.md](deploy/benchmark-policy.md). Historical checkpoint experiments use different layouts and must not be presented as cumulative gains. `tools/benchmark-fixed.py` compares the current stable or LTS image with a candidate, with paired process-cold and repeated requests, independent pixels, and origin-side GET/HEAD/byte counts. A same-image control identifies timing noise.

Latest is the current experiment. Stable advances for any verified improvement that is stable after correctness and regression checks. LTS advances whenever a verified concrete improvement of at least 10% is demonstrated in a substantial workload against current LTS under matched conditions, with the workload named explicitly and reproducible regressions investigated. Channels may share an image; retire superseded images promptly and update aliases, release metadata, benchmark policy and retention pins together. Continue generating offline UI/backend deployment ZIPs; do not upload them to websites or external storage.

OpenJPEG uses a bounded thread budget per request. Large ROI decodes can lease extra CPU workers; small native-resolution previews use synchronous decoding. The shared pool reserves one CPU per admitted decode slot and limits additional OpenJPEG workers to the remaining CPU allowance. `RIVER_OPENJPEG_THREADS_PER_DECODE` is a maximum: `auto` (default) allows up to four on hosts with at least eight CPUs, two with at least four CPUs, and one on smaller hosts. Explicit numeric values remain supported. Compact and throughput16 profiles retain their one-worker default unless overridden. CPU limits and concurrent work can reduce the actual allocation. The jpeg2k 0.10.1 wrapper is vendored only to add a per-request threads parameter; its pixel conversion and ROI behavior remain unchanged.

From 2026-10-22 the required performance gate covers three fixed physical image files: approximately50 MB,900-1000 MB and10-15 GB. Each has a full preview, nonblank deep tile and exact repeat. The v2 manifest, original hashes and current263 baseline are recorded in `deploy/benchmark-suite.json`; all future candidates must rerun the current stable alongside the candidate across all six cold requests. The recorded baseline is not a historical timing substitute for fresh paired comparisons. The first v2 control identified a slow11 GB deep-tile decode despite small source IO; that case remains in the gate.

Huge eligible single-native-tile sources now use an aligned temporary codestream window (`RIVER_S3_PRECINCT_REBASE=auto`). This avoids allocating metadata for the original enormous canvas when decoding a small ROI. The window preserves precinct/codeblock boundaries and the full-depth reversible5/3 support envelope; it keeps only entropy already selected by the ROI. Reduced preview passes use the same route, preventing large-canvas allocations during progressive delivery. Original source files stay byte-identical. Single-layer, zero-origin Part1 sources with a usable single-part PLT are eligible; SOP/EPH and other unsupported layouts retain ordinary paths. Automatic windows must reduce area at least4x and stay below256 million pixels, with original area at least64 million pixels. Set the environment value to false to disable.


### Source range diagnostics and pressure limits

Adaptive payload costs use the lower of the smoothed and latest valid body
throughput after sufficient measurements. Optional `refreshPayloadCosts: true`
in `PUT /api/settings/read-policy` samples a small already-required payload
range before a mature source's predicted overfetch. It is off by default:
tests found better response to sudden bandwidth loss but extra latency on a
steady fast source. `RIVER_S3_REFRESH_PAYLOAD_COSTS=true` supplies the ENV
default; an explicit persisted API value overrides it. Whole-configuration
export/import includes the effective value. Nearby sources and tight read
budgets retain the existing path; no synthetic probes are added.

`PUT /api/diagnostics/source-reads` with `{"enabled":true}` enables a process-local ring of at most 4,096 records. `GET` reads it and `DELETE` clears it. Tracing starts disabled; `RIVER_TRACE_SOURCE_READS=1` enables it at startup. Records include read-context and opaque object IDs, exclusive byte offsets, block indices, code callsites, validated SOT tile/part index entries, elapsed time and outcomes. No object URLs, keys, credentials or response bodies are retained. Evictions are reported explicitly.

Console logs use plain text without ANSI colors and split long field values into
lines of at most 160 UTF-8 bytes, including the correlation prefix. Each line carries `t` (Unix milliseconds), `e`
(process-local event ID), `r` (root span ID), and `s` (current span ID), so split
messages remain correlated. Full span payloads are not repeated on console lines;
OpenTelemetry spans retain their fields. `RUST_LOG` still controls log levels.
Missing TLM and missing PLT observations explain the alternate read plan at INFO;
these are missed optimizations, not declarations that the input is unsupported.
Both indexed and unindexed inputs are intended serving paths. TLM supplies
tile-part offsets; PLT supplies packet lengths. Neither is a format requirement.
Selected TLM lookup reports `index_source=tlm` and
`action=validate_selected_sot`; unavailable indexes report the reason and alternate
action. Unindexed discovery still obeys source read and memory budgets, so some
large requests currently fail within those limits rather than reading without bound.

Experimental `RIVER_S3_UNINDEXED_COARSE_PREFIX=true` enables a bounded candidate
for whole-image reduced previews up to 512 by 512 pixels of eligible reversible multilayer sources
without a usable tile-part index. It validates actual SOT and packet coverage;
part ordinals alone never prove resolution or quality. It can exploit a compact
run of early parts across tiles; incomplete-quality runs retain ordinary discovery. Once a
complete contiguous tile-part chain is discovered, the experimental path can
reuse it in the compact prefix planner rather than copy all native payloads.
The initial first-part candidate is capped at 8 MiB and 128 reads; subsequent
discovery retains the ordinary request limits. It is disabled by default and is not a
general solution for unindexed imagery. Source bytes remain unchanged.

Validated index records identify their path: `validatedSot` for serial discovery,
`validatedTlmSot` for selected TLM entries checked against SOT, and
`validatedConnectedSot` for speculative entries accepted only after their chain
connects to validated offsets. These metadata records are not GETs or transferred
bytes. Cache reuse need not emit new validation records; their absence alone does
not prove an index is absent. Check ring eviction before drawing conclusions.

`rangeRead` means a logical cache-miss operation. `wireAttempt` records each SDK HTTP send attempt, including retries and its response status, through response headers. A connection failure may happen before the server receives a GET. Native curl attempts are separate `nativeAttempt` records, including body transfer. Do not add logical reads and physical attempts together. Successful body bytes belong to logical operations; SDK attempt timing is time to headers. Cache hits produce no range-read record. Initial header probes have separate context/object IDs because the version identity is not known yet.

`tools/profile-range-reads.py` checks all three frozen source sizes, correlates callsites with enclosing Rust functions, reconciles counts/byte ranges against the independent fixture origin, checks reference pixels, and verifies repeated loads perform no source IO. Its timings are diagnostics, not paired release benchmarks. The fixture origin accepts `--delay-ms` for controlled distant-source tests.

`maxSourceRequests` in `/api/settings/read-policy` limits in-flight range operations across images on each connected source (1–64; default 16). The UI exposes the same setting. It persists in configuration export/import and across restarts. `RIVER_S3_SOURCE_MAX_REQUESTS` supplies the startup default. SDK retries retain their operation's admission slot; requests cancelled or timed out while waiting release admission. This is a concurrency limit, not an RPS limit. Existing JPEG 2000 byte/deadline/request budgets still apply. Fast-transport and parallel-range planning also consider recent observed latency rather than relying solely on the source's historical minimum.


Adaptive mode also uses bounded look-ahead for large segmented PLT headers when recent measured source latency is at least 5 ms. It derives an allocation-free packet-count hint from COD geometry, limits each window by the configured overfetch ratio, remaining request-byte budget, tile-part extent and 1 MiB header bound, then validates every marker normally. A private header window prevents repeated reads even with the shared range cache disabled. Observe/disabled mode, nearby sources, incomplete tile-part packet sets, small headers and an overfetch ratio of one retain ordinary segment reads. This optimizes packet-length-index loading; it does not eliminate SOT discovery for files without a tile-part index.

The read-policy source observations expose `activeReads` (currently admitted source operations, including retry/backoff) and `admissionWaits` (cumulative operations that queued at least once). Admission identity remains shared beyond128 connections. For range-trace records, `tilePart` identifies the requesting discovery cursor; speculative/batched ranges can span other parts. Map actual headers through validated `indexEntry` offsets.


Whole-image previews on eligible unindexed sources with measured range latency of at least2ms now spread the existing speculative-index budget over sixteen smaller windows. The budget ceiling and range concurrency are unchanged; deep/partial views keep the previous probe strategy, and RIVER_S3_INDEX_SHARDS remains an explicit override. Only SOT chains connected exactly to the validated main walk are committed. This improves discovery overlap without claiming a small GET count or modifying originals.


Sources with PLT tables reuse parsed immutable tile headers across requests on the same source version, including small tables and multiple native tiles. `RIVER_S3_PARSED_HEADER_CACHE_BYTES` bounds this shared cache (default 16 MiB; 0 disables), with a 4 MiB entry cap, 16,384-entry limit and exact tile-part identity checks. Header discovery consults this cache before walking markers. A cached parsed header also retains its validated SOT; TLM header probes may reuse that validation, while probes extending into packet payload and complete-part prefetch still read the requested bytes. Active leases remain valid after eviction. This preserves packet metadata when the compressed-byte cache is small or evicts header bytes.

Immutable packet geometry has a separate shared LRU controlled by `RIVER_S3_PACKET_PLAN_CACHE_BYTES` (default 64 MiB, maximum 256 MiB, 0 disables), capped at 512 entries. Its key includes exact tile bounds, coding parameters, progression, quality layers and requested prefix. It shares no source offsets or entropy bytes. Multipart preparation can reuse these descriptors across requests; RPCL precinct-window planning retains its streaming traversal. Both caches are process-local; active request leases are additional to retained-cache budgets. Neither changes source files. These preparation changes require HTTP performance qualification before release promotion.

Multilayer spatial-selection candidate: `RIVER_S3_PRECINCT_ROI=true` can also select precincts in compatible multilayer reversible Part 1 codestreams. Spatial selection keeps the requested quality layers of each needed precinct and omits unrelated precincts; the existing quality limit still applies. Multilayer coordinate rebasing remains disabled. Unsupported transforms, packed headers and coding overrides retain their existing eligibility checks and fallback paths. The initial synthetic ten-layer multipart test verifies exact corner/edge pixels at full and reduced resolution with four quality limits; it is not an organizational latency or release qualification claim.

Multipart payload demand can be planned across selected tile-parts instead of fetching each part serially. Aggregate plans are limited to 65,536 packet descriptions, the existing request metadata budget (`RIVER_S3_PREP_METADATA_BYTES`, default 4 MiB), 8 MiB of retained requested payload and 48 initial contiguous ranges. A plan outside these bounds falls back before aggregate payload I/O. Merged gap bytes are discarded from request-local retention; the existing source byte, request, overfetch and timeout limits remain in force. Temporary SOT markers are reconstructed from already-validated tile, ordinal and total fields with the new subset length, avoiding another source read solely to copy those fields. This does not rewrite source files. The separate three-size gate and multilayer diagnostic results are recorded in the deployment bundle's `PERFORMANCE.json`; a gain on the latter is not a claim of faster arbitrary large previews.

S3 connections accept `maxRetries` (0–10, retries after the initial attempt). Omitted values use `RIVER_S3_MAX_RETRIES`, default2, and the effective value is persisted with the connection. Both SDK and native serial transports honor this limit; range reads also retain their existing deadlines. A default failed range can therefore make at most three attempts rather than the SDK's former eleven. Retries consume source admission capacity, and physical attempt tracing remains separate from logical range counts. This bounds failure amplification; it does not reduce successful index-discovery reads or impose an RPS limit.

Physical attempt enforcement now accompanies the preparation-context range budget. SDK sends claim an atomic shared allowance before transport execution, including internal retries and parallel/speculative workers; native serial attempts claim the same allowance. Enforcement is independent of diagnostic tracing. Exhaustion prevents another send and returns a specific error. Initial probing and subsequent header parsing share an allowance; later rendering creates its own preparation context. This is not an aggregate cap for every phase of one HTTP request, nor an RPS limit. SDK response statistics still count logical ranges; use `wireAttempt` traces for physical attempt counts.

Source-read diagnostics include a server-generated `summary`, grouped by read context, opaque object ID, purpose and callsite. Logical reads, physical transport attempts, recorded body bytes and validated index entries remain separate; native logical parents are not counted twice. SDK HTTP status counts appear per group. `summary.partial=true` means the bounded ring evicted records, so totals cover retained records only. Incomplete transfers may have unknown attempt/body-byte counts; physical attempts do not prove arrival at the origin. The raw events retain individual ranges and indices. Summaries are computed only when diagnostics are fetched, outside the serving path.

Automatic payload scheduling uses up to16 lanes for single-native-tile packet batches of 1–8MiB with at least8 ranges and observed latency of at least5ms. Nearby sources, metadata reads, smaller batches and multi-tile grids retain their previous scheduling. This changes neither ranges nor source bytes. The shared per-source `maxSourceRequests` admission limit still applies; an explicit `RIVER_S3_RANGE_CONCURRENCY` value overrides the automatic choice. This trades a shorter burst within the existing source limit for fewer sequential network waves, rather than adding source requests.

For distant unindexed whole previews, automatic speculative seed layout accounts for frontier concurrency: sixteen smaller windows under a limit of four lanes or fewer, four wider windows with more lanes. The probe-byte ceiling is unchanged; partial views and explicit `RIVER_S3_INDEX_SHARDS` settings retain their behavior. This avoids applying the four-lane tuning to unconstrained frontiers. Header offsets are still accepted only after validation through the original tile-part chain.


Full-image progressive tile batches are classified by complete normalized tile-grid coverage at each level, including partial image-edge cells. This lets their required coarsest prefixes share index-discovery reads as viewport requests already do. Sparse batches, missing cells and duplicate/out-of-range coordinates do not establish full coverage. No additional spatial tiles or source bytes are requested by this classification.

For an already opened asset, `GET /api/assets/{asset_id}/manifest?includeStructure=true` opts into parsed codestream diagnostics. Ordinary manifest responses are unchanged. The structure reports native geometry, quality/resolution counts, fixed COD bytes, codeblock style and wavelet separately, startup read flags, packet-count geometry and any resident usable TLM summary. Inspection itself performs no source IO. Evicted metadata remains explicitly unknown; a TLM is not a packet-length index, and main-header candidate checks do not guarantee a particular tile request can be served.

Speculative SOT index probing waits for eight required preparation reads before classifying a source as persistently slow. A measured fast range can disable probing earlier. These are reads needed for the request, not extra latency-sampling requests. `GET /api/settings/read-policy` exposes `learning.indexDiscoveryCalibrationReads`. Existing probe-byte, request, concurrency and deadline limits remain in force.

`RIVER_S3_INDEX_ONLY_PREFIX=true` enables bounded reduced previews when single-native-tile PLT pointers are carried in zero-body tile-parts before packet data. The reader follows exact SOT/header chains, retains only the needed prefix lengths, and projects an entropy-preserving temporary view. It currently requires compatible one-layer, reversible resolution-first coding, zero origin, a full requested level no larger than4million pixels, and existing request/byte/metadata budgets. Later coding overrides cause fallback. Original images are unchanged.

Experimental index-only header windows: `RIVER_S3_INDEX_ONLY_HEADER_WINDOW_BYTES=1048576` opts into up to 1 MiB contiguous reads while validating index-only tile-part headers. Default `0` retains sparse marker reads. The window is capped at 8 MiB and all request byte, GET, deadline and source concurrency limits still apply. This can greatly reduce sequential GETs on files with large PLT tables, but can transfer tens of megabytes of header bytes instead of hundreds of kilobytes. It does not automatically follow the adaptive overfetch ratio: this is an explicit operator override for the experimental index-only path, not a general default. Original files remain unchanged. The optional persisted `indexHeaders` API policy described below takes precedence over this environment override.

The read-policy API now accepts an optional `indexHeaders` object, persisted with the complete configuration: `{"mode":"adaptive","windowBytes":1048576,"maxExtraBytes":33554432}`. Its modes are `sparse`, `dense`, and `adaptive`. Omit it to retain the legacy environment override. The API policy takes precedence over that override. Adaptive selection requires the configured minimum observations and at least three actual throughput samples; unknown sources stay sparse. It predicts serial marker-read cost versus a window and requires at least25% predicted savings. Global observe mode reports savings without applying windows. This is a prediction, not a timing guarantee. No synthetic calibration GETs are issued; prior genuine requests on the same connection train the source profile. Profiles are process-local. `sources.<id>.indexHeaderDecision` in GET /api/settings/read-policy explains the decision and its measurements. The byte allowance conservatively charges every window byte, not just gaps, and exhaustion resumes sparse reads. Existing source concurrency, request-byte, GET and deadline limits still apply. This metadata allowance is explicit and separate from packet `maxOverfetchRatio`. The experimental policy only applies to compatible index-only tile-part previews; it does not optimize every JP2 layout.

Windows larger than 1 MiB use streamed header prefetch: the physical range can be up to 8 MiB, but only up to256 KiB of marker-head and requested packet-prefix blocks are retained per window. Skipped PLT bodies are consumed from the network without entering the shared cache. The ordinary parser revalidates the exact marker chain, including later coding overrides. Short tail blocks are not cached as complete blocks. Shared-cache-disabled operation retains the bounded blocks for the request. The normal compressed-byte, physical-GET, metadata-memory, cancellation, deadline and source-admission budgets still apply. With an 8 MiB window, maxExtraBytes must cover at least one whole window;32 MiB is an explicit opt-in allowance, not a new default. GET /api/settings/read-policy reports the maximum window and retained-block sizes. This does not guarantee2-5 GETs: initial container/header reads, EOC checks and needed packet data are additional requests.

For TLM-indexed RLCP/RPCL sources, the multipart resolution-prefix path can retain all quality packets of the required resolutions even when the caller requests fewer quality layers. The temporary codestream preserves the original layer count and packet entropy; the decoder applies the requested layer limit. This avoids making a first-quality-layer preview ineligible for resolution-prefix reads merely because the source has multiple layers. PLT and supported inline packet headers are both handled. Partial-quality requests on objects at or below the existing RIVER_S3_SMALL_OBJECT_BYTES threshold (default8MiB) retain the established small-object path to avoid trading a small transfer for many header GETs. This does not guarantee a fixed GET count on fragmented layouts.

Large multilayer TLM prefix reads can propose up to three adjacent small tile-parts in one physical read, capped at16KiB. This applies only to the initial probe when the part count matches the resolution count; it is read-ahead, not an assumption about actual packet boundaries. Ordinary SOT, coding-marker and packet parsing still prove the prefix and can stop before prefetched bytes or continue past them. Noncontiguous or large parts retain their normal probes. Expansion is abandoned if the conservative byte allowance or8MiB request-local lease budget would be exceeded. Small-object behavior is unchanged.


## Historical 698 release qualification (2026-11-02)

Before744, Stable, LTS and Latest resolved to the unchanged698image (`river:2026.11.01-multipart-batch-combo-m698-trial`). The immutable tag retains its original trial name; `deploy/releases.json` records current qualification. Direct six-pair comparisons on the frozen synthetic multilayer workload reduced neighboring-tile latency from697 to260ms versus294 and658 to224ms versus458, with exact pixels and zero-read repeats. This qualifies LTS under the user's >=10% substantial-workload rule. Three-size primary timings remain mixed; no universal preview, organizational S3 or32coreoptimum claim is made. See `reports/kakadu-io-m303/channel-qualification-m737.json` for raw evidence pointers and regression review. Superseded local image versions are retired; archives and data/configuration volumes remain available. Future successful builds advance Latest automatically; Stable/LTS require qualification.


## Current release channels: 744 (2026-11-02)

Stable and LTS point to `river:2026.11.02-opaque-png-combo-m744-trial`. Latest follows `deploy/releases.json`. LTS qualifies on **lossless PNG response-byte reduction**, not latency: the six frozen cold requests transfer12.5-25% fewer output bytes with identical pixels and source reads. The11GBpreview falls5,170,488to4,521,891bytes.286unit tests,alpha/crop/padding checks,all three-size paired cold/repeat checks,and settings/cache-preserving698upgrade pass. End-to-end latency remains mixed; source discovery and organizational S3 qualification remain open. Fully opaque PNGs use RGB; alpha-bearing PNGs retain RGBA. Other output formats and source images are unchanged. The only production source delta is `crates/river-server/src/decoder.rs`; unrelated local experiments remain preserved. See `reports/kakadu-io-m303/opaque-png-release-m750.json`. Retention follows active channels and the two newest versions;698is the immediate previous image and has no permanent pin.


Latest 761 remains experimental: source/decode overlap passes294unit tests,independent clipped RGB16/MCT pixel checks,and real-worker cancellation/contention/error cleanup. Full HTTP confirmation on the frozen979MBpreview measured414to396ms median across12pairs,with identical349GETs/3.13MB,pixels,and encoded-cache repeats. This does not establish the standalone prototype's16%gain or a10%LTSlatency improvement. See `reports/kakadu-io-m303/openjpeg-overlap-decision-m765.json`. The feature is opt-in through `RIVER_OPENJPEG_PIPELINE=true`,requires at least4configured OpenJPEG workers,two spare CPU leases,a suitable cold RGB16single-layer source,and fast calibration from eight required reads. It is not enabled in the qualified744bundle. Rejected757was removed; images744and761remain.

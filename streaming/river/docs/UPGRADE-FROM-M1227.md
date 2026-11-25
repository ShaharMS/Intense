# Upgrade from m1227 to m1229

River m1229 ships one Linux amd64 image. The version tag is `localhost/river:m1229` and the three channel tags are `localhost/river:lts`, `localhost/river:stable` and `localhost/river:latest`. The portable archive installs all four tags. See BUILD-MANIFEST.json for the image ID and QUALIFICATION.json for the checks on that image.

## What changed for tile clients

- **Preview → full quality.** On eligible multilayer JPEG 2000 images, River collects the pieces of each requested codeblock across quality layers. OpenJPEG Tier-1 workers can start when a block is complete, while other S3 ranges are still arriving. Foreground decoding reuses completed coefficients or a compatible preview checkpoint. Unsupported layouts continue through their ordinary decoder path.
- **Shared work in RAM.** River keys reusable compressed data and decoder work to the source version. Overlapping requests can share in-flight reads and identical renders. The requested tile returns first, and useful read-ahead stays available for the next tile. A neighboring tile can still need more S3 bytes when its footprint extends beyond the cache.
- **Faster first preview.** Tile-index validation, marker reads and header prefetch reuse bytes from the initial source probe even after the payload cache selects a larger block size. This removes a serialized storage round trip on the measured multilayer fixtures.
- **Request diagnostics.** WMTS responses expose foreground stages in `Server-Timing`, including waits, preparation and rendering. Verbose debug summaries show request coordinates, cache routes, origin bytes, GETs and a timing breakdown. See SETTINGS.md for the logging switches.
- **Client migration.** Use ordinary HTTP WMTS GetTile requests for preview and full quality. WebSocket tile delivery, the legacy catalog and bucket browsing were removed. The optional Atlas map example at `/map` is separate from the tile API. Existing WMTS clients can keep their tile URLs.

The three multilayer originals in the local optimization benchmark received full quality a median **87.6 ms** after preview started. The build immediately before the probe fix took **91.2 ms**. All 30 neighbors needed zero new GETs. These figures compare two development builds under a controlled local origin. COMPARISON.html reports the separate packaged-image comparison with m1227.

The consolidation since m1227 removes the image catalog and its viewer. Rendering continues through
original S3 object URLs, tile-info, streamer tiles and WMTS.
The optional Redis/Valkey tier gains multiple endpoints. RAM-cache allocations gain
bounded workload learning. Explicit process memory budgets account for headroom
outside River’s share of a shared cgroup, avoiding false pressure from unrelated
processes while still respecting the overall group limit. No universal latency speedup is claimed.

## Existing installations

Preserve source IDs, private configuration, credentials, certificates and data/cache
volumes. Do not replace an existing configuration with the fresh-install example.
Record the old image digest and effective settings before switching traffic. Use
one qualified image for Stable, LTS and Latest; these labels no longer select
separate performance implementations. Qualification belongs to the exact digest.

Use the existing `river` Compose project. The local source checkout's
`tools/river-compose.py` retains the six permitted backend slot names. New portable
installations use `replica-1`; do not accidentally start a duplicate deployment over
existing occupied ports. Configure the existing service with the new image and
preserve its mount definitions. Retire superseded images only after checking their
container references and completing acceptance.

## Removed interfaces and paths

- `/api/assets`: removed; River does not enumerate available images.
- `/api/s3/sources/{source_id}/objects`: removed; no S3 listing/search.
- `/api/s3/sources/{source_id}/assets`: removed; registration is unnecessary.
- The legacy catalog viewer is removed; `/` redirects to `/docs/`. Source connection management
  remains available; listing configured connections does not list bucket contents.
- `RIVER_ASSET_CATALOG`, mosaic sidecars and startup discovery are retired.
- Local images live directly in `corpus/`. Use `local:<relative-key>` as the image
  parameter; it is constrained to `RIVER_CORPUS_ROOT`. Never pass arbitrary host paths.
- WMTS GetCapabilities without LAYER no longer enumerates retained image handles.
  Obtain tile-info for a known image and use the returned assetId as LAYER.
- WebSocket clients must migrate to HTTP tile requests. The `/ws`, `/ws/tiles`
  and `/api/ws-capabilities` endpoints have been removed.
- Remove `RIVER_WMTS_BATCH_SIBLING` from deployments. River now renders neighboring
  tiles on demand. A viewer can preload the leading edge using ordinary GetTile requests.
- Retire WebSocket-only viewport concurrency, preview patch, pixel reuse, refinement delay
  and progressive prefix lookahead settings. HTTP clients can request preview and full
  quality separately with WMTS.
- All generated output belongs under `target/`: `dist/` for release ZIPs,
  `deploy/` for assembled bundles, `fixtures/` for generated manifests,
  `benchmarks/` for raw measurements, and `builds/` for frozen builds.

The prepared-image cache and versioned locator cache remain internal, bounded,
demand-loaded caches. They are not a catalog and never enumerate S3.

## New settings

`RIVER_CACHE_LEARNING=true|false` / JSON `cacheLearning` enables automatic
redistribution. It defaults to true. `PUT /api/settings/cache-learning` with
`{"enabled":false}` changes the running policy; DELETE restores startup policy.
These mutations require `RIVER_ADMIN_TOKEN`. Disabled adaptation also pauses
learning. Dedicated live changes reset on restart; use ENV/JSON for durable policy.

`RIVER_SHARED_CACHE_ENDPOINTS` / JSON `sharedCache.endpoints` accepts an array:

```json
[{"id":"cache-a","url":"valkeys://cache-a:6379","weight":1},
 {"id":"cache-b","url":"rediss://cache-b:6379","weight":2}]
```

Keep IDs stable through credential rotation. Weights represent relative key
placement, not memory reservations. Configure each server's own memory ceiling,
eviction and availability separately. River does not automatically replicate
entries or implement Redis Cluster redirections. Each endpoint is an independent
RESP server. Existing single `url` / `RIVER_SHARED_CACHE_URL` remains supported;
choose one form. The endpoints ENV setting takes precedence if both ENV forms exist.
A live shared-cache replacement validates all connections before switching.

JSON supplies startup values, ENV overrides those, and dedicated live APIs win in
the running process. ENV is reapplied on restart. Shared-cache credentials are
omitted from diagnostics and generic exports. Do not commit credentials or embed
them in deployment archives.

## Acceptance and settings to inspect

1. Check `/openapi.json` for the build version and the container's image digest.
2. Check `/api/drivers`, `/api/settings/drivers` and actual decoder response headers.
3. Check `/api/diagnostics/resources`: effective budget, occupancy, active memory,
   storage mode, cache-learning state and external-cache endpoints.
4. Check `/api/settings/read-policy`: Observe measures; Adaptive applies planning.
   Preserve explicit request and overfetch limits; do not recreate sources to tune.
5. Render a real production image cold, after a preview, and as an exact repeat.
   Check pixels, latency, origin GETs/bytes and actual decoder/fallback.
6. Verify your actual TLS protocols, certificates, CORS origin and HTTP tile client.
7. Exercise an external-cache outage: bounded failures must fall back to the source.

Read QUALIFICATION.json for what was actually exercised on this image. Synthetic
fixtures isolate layouts and correctness; they do not replace production acceptance.

Cache allocation now weights measured producer work reused per resident byte, with
coverage, reuse and eviction pressure. Unknown index/metadata time benefits remain
explicit in diagnostics. Per-source measured origin/needed bytes feed bounded
optional gap planning in adaptive mode. See CACHE-GUIDE.md for coverage, exclusions,
warmup, hysteresis and why these counters are not standalone speedup claims.

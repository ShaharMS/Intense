# Upgrade from m1229 to m1230

River m1230 ships one Linux amd64 image under the versioned tag and the stable, lts and latest tags. The exported image, rollout commands, exact image ID and local test results are in this bundle.

## Existing OpenShift installations

Change the image on your existing workload. Keep your source definitions, Secrets, routes and resource settings. For the RAM-only profile keep `RIVER_AUTO_ADAPTATION_MODE=liberal,no-disk` and your existing `RIVER_CACHE_LEARNING=true`. No new environment variable is required. Leave scratch and cache sizes unset for automatic sizing. Explicit caps continue to take precedence. DEPLOYMENT.md includes the commands to import the archive, push the versioned image and roll out by registry digest. Keep the previous digest for rollback.

## Changes since m1229

- **Memory admission and eviction.** Concurrent decode reservations now account for memory before workers touch the pages. Waiting workers retry reclamation as active readers finish, and freed allocator pages can return to the OS. Cache eviction diagnostics no longer keep an evicted range allocation alive. Slot and byte reservations use the same lock to avoid a missed wakeup. A genuine memory admission timeout returns HTTP 503 with code `memory_admission_timeout`. Clients that handled the old 422 as a permanent image error should distinguish this retryable condition and back off.
- **RAM-only compatibility decoding.** OpenJPEG and native Grok can consume a complete compatible source from process memory, with the scratch reservation held for the lifetime of that buffer. Those paths can run without a custom shared-memory mount. File-only paths that need named scratch files still need writable tmpfs. A pod can still reject a decode whose working set exceeds its budget.
- **Neighboring tiles.** Requests that need the same complete native tile part can share its prepared decoder input in flight and later from the versioned range cache. The cache charges this input against its existing byte budget. Eligibility depends on source layout and whether the entire input is reusable.
- **Decoder selection.** Automatic selection can use the native Grok bridge for eligible 512-pixel full-resolution requests on lossless 8-bit RGB LRCP sources with deep resolution pyramids and enough decoder threads. Explicit driver selection still takes precedence. Existing imagery requires no re-encoding.
- **Large S3 objects.** A required complete native tile part can use more of the memory already budgeted for its I/O lane. The automatic selective-read allowance for eligible large objects can grow from 64 to 128 MiB. An explicit `RIVER_S3_REQUEST_BYTES` still wins. `RIVER_S3_COMPATIBILITY_BYTES` controls the separate whole-object fallback ceiling, which defaults to 2 GiB. Raising that ceiling can make one tile require the whole source in memory.
- **WMTS diagnostics.** With `RIVER_LOG_LEVEL=debug` or `trace`, responses include detailed preparation and render timings even with request-summary logging disabled. Read `Server-Timing`, `X-River-Source-Stages` and `X-River-Index-Source` to identify index discovery, payload transfer, cache reuse and decoder work where those stages ran. Parent stages include their children, so adding every duration overcounts wall time.

COMPARISON.html reports the local m1229 and m1230 image measurements. QUALIFICATION.json identifies the image, conditions and checks. Cold index discovery and first full-quality reads on coarse native tiles can still dominate latency.

## Measured WMTS response times

The same generated fixtures and tile coordinates ran against each release image on a loopback S3-compatible origin with 4 ms delay and 512 MiB/s bandwidth. Values are median client response times across two fresh server starts per build. The six-tile bursts measure each tile from submission to response. These are local measurements, and the two-run figures for individual tiles are approximate.

| Generated fixture and operation | m1229 | m1230 |
| --- | ---: | ---: |
| 12-level terrain with TLM, preview | 56 ms | 46 ms |
| Same tile, first full quality after preview | 767 ms | 639 ms |
| Next full-quality neighbor | 296 ms | 67 ms |
| Same image, six-tile pan burst after one starting tile | 967 ms | 246 ms |
| 11-level terrain without TLM, six-tile pan burst after one starting tile | 165 ms | 199 ms |

All 336 WMTS responses per build matched the generated pixel references. The 11-level pan result is a remaining regression on this workload.

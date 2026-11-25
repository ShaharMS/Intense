# Settings to learn when upgrading

Start with UPGRADE-FROM-M1227.md. Keep the existing configuration and credentials.
Startup reads JSON, then ENV overrides it. Dedicated live APIs take precedence in
the running process. Not every live setting persists: cache-pool, adaptation and
cache-learning changes are process-local; put durable startup choices in ENV/JSON.
TLS, logging and sizing overrides require a restart. Importing a whole configuration
replaces its source list; never import an empty example over existing connections.

## New in this consolidation

| Setting | Purpose | Live control |
| --- | --- | --- |
| `RIVER_CACHE_LEARNING=true` | Bounded workload-specific RAM-cache redistribution; explicit cache limits stay fixed | GET/PUT/DELETE `/api/settings/cache-learning` |
| `RIVER_SHARED_CACHE_ENDPOINTS` | JSON array of independent Redis/Valkey endpoints, each with stable `id`, secret `url`, optional `weight` | GET/PUT/DELETE `/api/settings/shared-cache` |

The new controls complement existing settings; they do not override source-request
limits or CPU budgets. Learning transfers at most 5% of baseline per minute and
requires measured work saved with coverage, demonstrated reuse and eviction pressure. It is not a latency oracle.

```dotenv
RIVER_SHARED_CACHE_ENDPOINTS=[{"id":"cache-a","url":"valkey://cache-a:6379","weight":1},{"id":"cache-b","url":"redis://cache-b:6379","weight":1}]
RIVER_SHARED_CACHE_TIMEOUT_MS=8
RIVER_SHARED_CACHE_TTL_SECONDS=3600
RIVER_SHARED_CACHE_MAX_VALUE_BYTES=2097152
RIVER_SHARED_CACHE_MAX_FILLS_PER_SECOND=128
RIVER_SHARED_CACHE_NAMESPACE=river
RIVER_SHARED_CACHE_TILES=true
RIVER_SHARED_CACHE_INDEXES=true
```

For TLS to the cache use `valkeys://` or `rediss://`. Supply authentication through
protected environment/secret mounts as appropriate. Keep endpoint IDs stable.
Do not configure `sharedCache.url` and `sharedCache.endpoints` together in JSON.
`RIVER_SHARED_CACHE_URL` remains the single-server option; if both ENV forms exist,
the endpoints array wins. A failed live replacement leaves the old pool active.
The cache servers need their own memory/eviction policy. River's queue limits and
fill rate are shared across endpoints; each endpoint still costs worker connections.

## Existing controls retained

| Area | Main ENV settings | Inspect/manage |
| --- | --- | --- |
| Configuration | `RIVER_CONFIG_MODE`, `RIVER_CONFIGURATION_FILE`, `RIVER_ADMIN_TOKEN` | `/api/settings/configuration` |
| Storage connections | `RIVER_S3_SOURCE_COUNT`, one-based `RIVER_S3_SOURCE_1_ID`, `_NAME`, `_BUCKET`, `_REGION`, `_ENDPOINT`, `_ACCESS_KEY_ID`, `_SECRET_ACCESS_KEY`, `_SESSION_TOKEN`, `_PREFIX`, `_ALLOW_HTTP`, `_VIRTUAL_HOSTED_STYLE` | `/api/s3/sources` manages connections only; no bucket listing |
| Drivers | `RIVER_ENABLED_DRIVERS`, `RIVER_ENABLE_NVIDIA` | `/api/drivers`, `/api/settings/drivers` |
| Adaptation | `RIVER_AUTO_ADAPTATION_MODE` | `/api/settings/adaptation` |
| Overall resources | `RIVER_MEMORY_BUDGET_BYTES`, `RIVER_CACHE_MEMORY_BYTES`, `RIVER_DECODE_CONCURRENCY`, `RIVER_SOURCE_IO_CONCURRENCY` | `/api/diagnostics/resources` |
| Per-cache ceilings | `RIVER_S3_RANGE_CACHE_BYTES`, `RIVER_S3_HEADER_CACHE_BYTES`, `RIVER_S3_INDEX_MEMORY_BYTES`, `RIVER_S3_PARSED_HEADER_CACHE_BYTES`, `RIVER_S3_PACKET_PLAN_CACHE_BYTES`, `RIVER_S3_PREPARED_PREFIX_CACHE_BYTES`, `RIVER_NATIVE_TILE_CACHE_BYTES`, `RIVER_RASTER_CACHE_BYTES`, `RIVER_ASSET_METADATA_CACHE_BYTES` | Configured limits, effective targets, occupancy and retained work are distinct |
| Local/storage paths | `RIVER_CORPUS_ROOT`, `RIVER_S3_CACHE_ROOT` | Use `local:<relative-key>` for known local images |
| Source planning | `RIVER_S3_ADAPTIVE_READS`, `RIVER_S3_SOURCE_MAX_REQUESTS`, `RIVER_S3_SERIAL_TRANSPORT` | `/api/settings/read-policy`; separate planned ranges, measured payload/metadata amplification and bounded feedback; `RIVER_S3_MINIMUM_SAMPLES` and `RIVER_S3_MAX_OVERFETCH_RATIO` bound learning |
| Output | `RIVER_WEBP_METHOD` and existing encoding settings | `/api/settings/encoding`, `/api/formats` |
| Console logs | `RIVER_LOG_LEVEL`, `RIVER_LOG_REQUEST_SUMMARY`, `RIVER_LOG_VERBOSE`, `RIVER_LOG_UNPROCESSABLE_LEVEL`, `RIVER_PRETTY_PRINT_LOGS` | Startup configuration; one summary per HTTP request by default |
| Resource display | `RIVER_RESOURCE_DISPLAY=auto\|color\|unicode\|ascii\|off` | Startup display; same resource state remains available through the API |
| Telemetry | `RIVER_OTEL_ENABLED`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME` | Existing collector infrastructure remains separate |
| CORS | `RIVER_CORS_ORIGINS` | Existing wildcard/explicit-origin contract is retained |
| TLS | `RIVER_TLS_ENABLED`, `RIVER_TLS_BIND`, `RIVER_TLS_PROTOCOLS`, `RIVER_TLS_CERT_FILE`, `RIVER_TLS_KEY_FILE`, `RIVER_TLS_PFX_FILE`, `RIVER_TLS_PFX_PASSWORD_FILE`, `RIVER_HTTP3_PUBLIC_PORT` | Mount certificates read-only; TLS changes require restart |

Adaptation modes are `disabled`, `conservative`, `liberal`, or comma-separated
`conserve-ram`, `conserve-disk`, `conserve-compute`, `no-disk`. `no-disk` is a hard
override: caches and bounded decoder scratch use RAM, with no disk fallback.
Without an attached cache volume, automatic storage detection also chooses RAM-only.
Do not confuse the configuration/certificate mounts with a cache volume.

Liberal retention uses the RAM left after scratch, native decoder, request, runtime and temporary reserves. The automatic plan keeps these reserves across the 32 GiB boundary. Under pressure River checks visible constrained cgroup ancestors as well as process RSS. At 85% of the effective allowance it reduces cache targets progressively. At 92% a half-second pressure check triggers emergency reclamation and reduces targets to 10%, including the decoder's coefficient and checkpoint caches. New decodes account for active workspace reservations. Cache entries held by an in-flight request can remain live until the reader releases them.

HTTP tile Auto chooses among enabled, available paths.
Read `x-river-decoder-backend` for what actually
ran. OpenHT may use Grok fallback; declared alpha uses a correctness-preserving path.
GPU availability requires compatible hardware/runtime, not merely an enabled flag.
The synthetic qualification cannot validate a GPU absent from the test host.

## What to inspect after deployment

- Resource allowance matches pod/container limits, and explicit constraints remain effective.
- RAM-only mode and scratch ceiling are correct for mounts and shared-memory capacity.
- Per-cache hits, evictions, average retained item size and learned targets reflect the workload.
- External endpoint metrics describe whole servers, not exact River namespace size.
- Origin GETs/bytes and actual decoder remain sensible for a cold image, a zoom after
  preview, and a repeat. A preview saves metadata/offset work but may not fetch zoom payload.
- OpenAPI version matches BUILD-MANIFEST.json and the deployed image digest.

Do not sum avoided-work estimates across nested caches. Do not infer a speedup from
cache size or hit count alone. QUALIFICATION.json and the comparison report explain
what was measured and what still needs production acceptance.

### WMTS request timings and debug summaries

Successful tile responses include a `Server-Timing` header with consecutive foreground
wall times in milliseconds. A cache hit can skip source preparation and rendering.
`shared_wait` measures time acquiring the lock for an identical render, including a
refinement already in progress. `source_admission` and `cpu_admission` measure
admission waits. `prepare` includes source reads and compressed subset assembly.
`render` covers the engine call for the tile batch. The existing
`x-river-decode-ms` and `x-river-encode-ms` headers provide codec details for the
returned raster. Adding these detail timers to the foreground stages double counts
work. Background refinement and client-side transfer have their own timelines.

Enable the multiline console summary with these startup settings.

```sh
RIVER_LOG_LEVEL=debug
RIVER_LOG_VERBOSE=true
RIVER_LOG_REQUEST_SUMMARY=true
```

Each tile summary includes the coordinates, output dimensions, decoder and cache
routes, compressed bytes read, cache-hit bytes, range requests, and each foreground
stage as a percentage of elapsed HTTP processing. A dedicated thread formats and
writes console events. The queue holds 1,024 events. When full it drops incoming
console events and reports the skipped count when output resumes. Event field
capture still consumes CPU on the producer. OTLP log export uses its own batch
processor.

### Diagnosing a slow WMTS request from pod logs

Set `RIVER_LOG_LEVEL=debug`. With request summaries enabled, the HTTP completion record contains the details. With summaries disabled, a timed tile emits a debug record labeled `WMTS tile diagnostic`. Verbose mode changes the console presentation. The same request ID ties the WMTS coordinates, decoder route, timings and I/O counters together.

For `s3-multipart-partial-compact-prefix`, River builds a reduced-resolution decoder input from selected native tile parts. The prefix plan records source geometry, native tile count selected, resolution reduction, quality layers, progression order, index entry count, read waves, parts with usable packet-length tables (PLT), tables rejected because their lengths did not cover the current part, parts that required inline parsing, and the resulting compressed bytes. A built partial prefix is ineligible for the complete-prefix cache. Range and raster caches have separate policies.

Use the per-stage GET, origin-byte and cache-byte counters with the matching wall times. `source_prefix_wave_fetch` covers batches of part-header probes. `source_prefix_probe_read` covers individual inline packet probes, including cache access and waiting for data. Compare its duration and GET count with `packet_parse_ms`, `source_prefix_payload_fetch` and `source_prefix_build`. `max_probe_ms` is the slowest individual inline read. Parent timings include their children. The final plan counters describe a completed prefix. An attempted plan can fall through or fail, leaving only completed stages. These diagnostics use the existing bounded console queue, which can drop records if the log sink stalls.

For a multipart inline prefix, `prefetched_inline_parts` counts selected parts requested together with the header wave after the preceding part required inline parsing. This trades some possible overfetch at the last packet boundary for fewer dependent GETs. Per-part lengths, packet order and selected resolution are still validated before decoding. Explicit per-request byte or range caps retain the narrow reads.

### WMTS preparation and decode failures

For a failed tile, use the response JSON `requestId` to locate the pod log record. The stable error `code` identifies the class of failure. The `message` carries the cause chain. For preparation or rendering failures, `details` includes the stage, selected driver, tile coordinates, reduction, layers, source dimensions, manifest component count and bit depth. During render it also identifies the input route. The corresponding log fields are `failure_reason` and `failure_details`. Tile failures use the configured 422 or server-error logging level even when ordinary request summaries are disabled. Debug mode adds timing detail.

OpenJPEG failures can include the failing native operation and the codec's bounded error text. If decoding succeeded and display conversion failed, the OpenJPEG area path reports the decoded color space and plane layout. An unlabelled two-band gray or unspecified-color source uses its first band for opaque grayscale display. Declared gray-plus-alpha retains transparency. Band count alone does not identify the sensor or wavelength.

At the default info threshold, a WMTS failure record includes the request ID, HTTP status, error code and reason. Render and preparation failures include the asset version, WMTS parameters under `replay`, effective reduction and layers, grid dimensions, driver selection and input route when known. Reissue the GET against the same asset version using those parameters, omitting null values. Compatibility requests also record their geometry override. S3 header failures include the connection ID, source size, version and manifest locator, including when the image fails before registration.

With `RIVER_LOG_LEVEL=debug`, the same failure also records the manifest, resident source structure (per-component precision, sign and sampling, JP2 channel definitions and color specification where available), worker budgets, storage and read policy, preparation counters, and completed stage timings as available. The structure inspection uses resident metadata and performs no new source reads. Raw console output works with both verbose logging and ordinary request summaries disabled. OpenShift determines how many screen lines a record wraps onto. The nonblocking console reports dropped events when its queue fills.

OpenJPEG keeps up to 2 KiB each of error, warning and informational text per codec. The native Grok bridge identifies the failing operation and captures up to 4 KiB of messages attributable to the calling decode thread. The RAM CLI fallback captures its exit status and up to 4 KiB of stderr, including an explicit empty indication. The raw log bounds the combined reason at 16,384 characters. Environment credentials and raster samples are outside the failure record. JPEG 2000 channel definition type 65535 indicates an unspecified role. For two-band grayscale display River uses the first component in that case. Type 2 represents premultiplied opacity and River converts those samples to straight alpha for PNG.


Set `RIVER_REQUIRE_CHANNEL_LABELS=true` to require an explicit JP2 channel definition for every channel during header discovery. A missing table, an omitted band, or an unspecified role or association produces a 422 with a `channel_layout` diagnostic, even for a single-band image or an otherwise usable RGB composite with an undefined fourth band. A colour-space declaration alone does not satisfy this opt-in rule. Raw JPEG 2000 codestreams have no JP2 channel definitions. With the setting unset or `false`, River uses the defaults below. Strict mode takes precedence over `RIVER_UNDEFINED_FOUR_BAND_LAYOUT`. Restart the deployment to change either setting.

For sources without usable channel assignments, River displays one band as grayscale, two bands as opaque grayscale from band 1, and three as RGB in source order (or grayscale for a declared gray space). For four bands, `RIVER_UNDEFINED_FOUR_BAND_LAYOUT` accepts `rgb` (default), `bgr`, `rgba`, `bgra`, or `reject`. RGB/BGR ignore the fourth band for display. RGBA/BGRA use it as straight opacity. A declared grayscale colour space keeps grayscale display. The option applies across the deployment; restart River to change it. It also applies to four-component raw codestreams. An undefined band carries no wavelength information.

Explicit JP2 associations take precedence, including BGR with an unspecified fourth channel. Conflicting assignments still produce a `channel_layout` diagnostic. Five or more source components require usable metadata. A 4–16 component sRGB or grayscale JP2 can omit any subset of auxiliary channels when all display colours are explicitly identified. River follows those colour assignments and any declared whole-image opacity. Without declared opacity it renders opaque pixels. Strict mode also requires labels for the remaining channels. Multiband ICC/palette transforms and per-colour opacity require an external display conversion. The structure diagnostic exposes `assumedDisplayLayout` when River applies an explicit fallback.

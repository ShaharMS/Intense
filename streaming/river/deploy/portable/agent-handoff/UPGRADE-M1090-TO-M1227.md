# Upgrade from m1090 to m1227

Use this guide with [DEPLOY.txt](DEPLOY.txt) for installation commands and [AGENTS.md](AGENTS.md) for deployment handoff. This bundle ships the qualified m1227 Stable/LTS backend. [PERFORMANCE.json](PERFORMANCE.json) records the tested workloads and inherited qualification evidence. [BENCHMARK-POLICY.md](BENCHMARK-POLICY.md) explains how to reproduce those comparisons.

## Preserve the running deployment

1. Record the running image digest, listener and proxy configuration, mounted volumes, and effective read policy. Keep the previous image available for rollback. If you export configuration through `/api/settings/configuration`, store the export securely because it includes S3 credentials.
2. Reuse the existing Compose project, config and cache volumes, environment, and source IDs. Apply only the settings you intend to change. Use the example files for their documented scenarios.
3. Bring up m1227 and verify the image digest, health, source connectivity, and a real client tile before switching traffic. Roll back using the previous image and its settings if validation fails.

## Configuration, S3 and resource sizing

- **m1095–m1109**. On `GET /api/settings/read-policy`, save `configuredSettings` for restoration and use `settings` to inspect effective values. Keep explicit per-source request caps. When comparing cold starts, record whether the client preconnected and separate asset registration, the source header probe, and frame delivery. Preserve the locator and metadata cache volumes to retain restart reuse.
- **m1185**. JSON supplies the base configuration, ENV overrides it at startup, and a live settings API change takes precedence in the running process. Use `RIVER_CONFIGURATION_FILE`, `RIVER_CONFIG_MODE`, `RIVER_S3_SOURCE_COUNT` with indexed `RIVER_S3_SOURCE_<n>_*` settings, and `RIVER_ENABLED_DRIVERS` as applicable. In ENV-only mode, make durable changes in the deployment environment. In JSON mode, startup ENV precedence applies again after restart. Inspect `/api/settings/drivers` and `/api/drivers` to confirm configuration and availability.
- **m1206 catalog**. Use bounded pages on `/api/assets` and `/api/s3/sources/{source_id}/objects`. Carry the returned cursor forward with the same source, `keyPrefix`, and `q`. Use a known key prefix to narrow origin work and let users request additional pages. Use direct asset registration when you already know an object key.
- **m1208 storage**. With no writable attached cache volume, River automatically uses RAM-only caches and bounded RAM-backed decoder scratch. The `no-disk` adaptation constraint forces this behavior even with a cache mount. Check the effective storage mode and scratch limit in `/api/diagnostics/resources`. For read-only pods, see [RAM-ONLY-DEPLOYMENT.md](RAM-ONLY-DEPLOYMENT.md) and the included Compose override.
- **m1214 adaptation and shared cache**. Use [RESOURCE-SIZING.md](RESOURCE-SIZING.md) for CPU and memory limits and [ADAPTATION-AND-SHARED-CACHE.md](ADAPTATION-AND-SHARED-CACHE.md) for presets, combined conservation settings, live overrides, Redis or Valkey, and startup resource display. Verify local cache occupancy and pressure per replica. Shared cache reduces duplicate work for supported entries. It has its own budget and availability.
- **m1217 shared progressive delivery**. With Redis or Valkey enabled, a cold replica can look up versioned encoded tiles in batches. Measure asset registration separately from frame I/O. Source version validation can still issue an origin probe. Test both a shared-cache hit and an unavailable cache service.

## Hosting and operations

- **m1185–m1190 client contract and TLS**. Use [STREAMER-COMPATIBILITY.md](STREAMER-COMPATIBILITY.md) for original URLs, tile-info, WMTS, and WebSocket integration. Configure allowed browser origins with `RIVER_CORS_ORIGINS` as appropriate. [NATIVE-TLS-DEPLOYMENT.md](NATIVE-TLS-DEPLOYMENT.md) covers mounted PEM or PFX certificates and protocol selection for HTTP/1.1, HTTP/2, and HTTP/3. Publish UDP for HTTP/3 and verify certificate trust and CORS using the actual browser origin.
- **m1206 and m1214 logs**. Use `RIVER_LOG_REQUEST_SUMMARY`, `RIVER_LOG_VERBOSE`, `RIVER_LOG_LEVEL`, and `RIVER_LOG_UNPROCESSABLE_LEVEL` to tune request logging. `RIVER_PRETTY_PRINT_LOGS` formats the console. Enable structured OTLP log export with `RIVER_OTEL_LOGS_ENABLED` and configure the collector as shown in [ADAPTATION-AND-SHARED-CACHE.md](ADAPTATION-AND-SHARED-CACHE.md). Use `/api/diagnostics/source-reads` for short targeted traces.
- **Decoder availability**. Use the GPU Compose override and `RIVER_ENABLE_NVIDIA` when deploying with an NVIDIA device. Check `/api/drivers` on the running instance. Record the actual decoder or cache from HTTP `x-river-decoder-backend` or WebSocket `serverTiming.decoderBackend`. A requested OpenHTJ2K route can fall back to Grok.

## Cold reads and progressive delivery

- **m1121–m1174**. Inherited cold-read changes overlap EOC validation with eligible PLT reads, reuse persisted index metadata after restart, and reduce origin work for eligible indexed overviews. Measure fresh-process, metadata-warm, and repeated-view runs separately. Verify exact pixels and record discovery GETs, payload GETs, and bytes.
- **m1184 and m1220 HTTP Auto**. Eligible indexed RGB zoom tiles and eligible single-native-tile lossless sRGB deep tiles can use native Grok. Test the actual backend with the installed and enabled driver list. Explicit driver requests remain available.
- **m1195–m1202 and m1225 scheduling**. Eligible typed-TLM layouts can prioritize smaller preview batches, look ahead to bounded future prefixes, and widen range dispatch within the effective source cap. m1225 qualifies the wider progressive path on the 979 MB indexed workload at a 16-request cap. Keep explicit `RIVER_S3_RANGE_CONCURRENCY` and source caps. Compare first frame, full preview, and final delivery separately under matched conditions.
- **m1219 and m1224 deep zoom**. Eligible reads can overlap the final EOC check and initial PLT read, or combine a bounded indexed header probe with parallel cold sampling. Measure the same zoom both cold and after a preview to see cross-request reuse. See the cold indexed zoom notes in [ADAPTATION-AND-SHARED-CACHE.md](ADAPTATION-AND-SHARED-CACHE.md).
- **m1227 WebSocket Auto**. Use `GET /api/ws-capabilities` to discover support. Auto can select native Grok for an eligible small, lossless indexed sRGB overview. The large indexed control used OpenJPEG. Verify explicit driver selection and the enabled-driver configuration. The verified gain applies to the tested 55 MB viewport and origin conditions.

## Deployment acceptance

- Verify the bundle checksums and compare the running image digest with [BUILD-MANIFEST.json](BUILD-MANIFEST.json). This image embeds the earlier `m77` OpenAPI version label. The included [openapi.json](openapi.json) is the corrected schema for this exact m1227 image.
- Check the effective resource plan, storage mode, source request cap, enabled drivers, catalog pagination, and a direct S3 tile. If applicable, verify native TLS negotiation, browser CORS, WebSocket delivery, and shared cache behavior.
- Run a cold overview, the same zoom before and after preview, and a progressive viewport against representative images. Record actual backends, reference pixels, origin I/O, first frame, preview coverage, and final delivery. Use [ORGANIZATIONAL-S3-PROFILE.md](ORGANIZATIONAL-S3-PROFILE.md) for deployment-specific measurements.

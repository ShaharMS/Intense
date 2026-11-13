# Resource adaptation and shared cache

Use `GET /api/diagnostics/resources` for detected limits, effective cache
targets, occupancy, pressure, storage mode and external cache metrics. The
startup chart shows these budgets separately from the external service.

## Adaptation

Set `autoAdaptationMode` in JSON or `RIVER_AUTO_ADAPTATION_MODE` in ENV.
The supported presets are `liberal`, `conservative` and `disabled`.
Alternatively combine `conserve-ram`, `conserve-disk` and
`conserve-compute`. Dimensions omitted from a combination use liberal
targets. Add `no-disk` to any valid combination. Unset preserves the
existing automatic baseline. The old `RIVER_RESOURCE_ADAPTIVE=false`
still selects disabled when no new setting is provided.

Explicit cache settings and admission ceilings take priority over policy
targets. Hard memory admission still applies when rebalancing is disabled.
Conserving RAM starts at a smaller target and grows in bounded steps under
observed demand. Conserving disk disables optional persistent cache
retention. Use `no-disk` to also forbid disk-backed decoder scratch. A
startup no-disk constraint stays in force during live changes.

Use `PUT /api/settings/adaptation` with a body such as
`{"mode":"conserve-ram,conserve-compute"}`. DELETE clears the override
and restores startup policy. Live overrides apply to future admissions and
cache targets. They are process-local and reset on restart. Set ENV or JSON
for durable changes. Mutation endpoints require the bearer token set in
`RIVER_ADMIN_TOKEN`. Worker pools configured at startup retain their
construction limits until restart.

## Redis and Valkey

The optional shared tier serves versioned single encoded tiles and compact
validated technical indexes or index prefixes. Local cache hits skip the network. A shared
tile hit can still require an S3 header probe to establish the object
version. Temporary files, compressed blocks and decoded pixel planes remain
local. Redis and Valkey share the same RESP client.

| Environment | JSON in sharedCache | Default |
| --- | --- | --- |
| RIVER_SHARED_CACHE_URL | url | unset, disabled |
| RIVER_SHARED_CACHE_NAMESPACE | namespace | river |
| RIVER_SHARED_CACHE_TIMEOUT_MS | timeoutMs | 8 |
| RIVER_SHARED_CACHE_TTL_SECONDS | ttlSeconds | 3600 |
| RIVER_SHARED_CACHE_MAX_VALUE_BYTES | maxValueBytes | 2097152 |
| RIVER_SHARED_CACHE_MAX_FILLS_PER_SECOND | maxFillsPerSecond | 128 |
| RIVER_SHARED_CACHE_TILES | tiles | true |
| RIVER_SHARED_CACHE_INDEXES | indexes | true |

Use `redis://`, `rediss://`, `valkey://` or `valkeys://` for a
single endpoint. Use the TLS variants for authenticated encrypted
connections. Keep credentials in a Secret. Use a namespace unique to the
deployment or tenant. Protect the shared service with an eviction policy
and memory ceiling configured on the service itself.

The cache reports hits, misses, failures, successful fills, dropped fills
and bytes transferred. Server-reported memory and key counts include all
clients on that server. River does not scan keys to estimate namespace
size. Network errors are misses. Fills use a bounded background queue and
can be dropped under pressure.

Use `PUT /api/settings/shared-cache` with
`{"settings":{"url":"valkeys://cache.example:6379","namespace":"river-prod"}}`.
River validates the replacement before switching. Use
`{"settings":null}` to turn the tier off live. DELETE restores the
startup setting. Credentials are not returned in resource diagnostics or
the generic configuration export. These controls do not support Redis
Cluster routing.

## Console presentation

Set `RIVER_PRETTY_PRINT_LOGS=true` or JSON `logging.prettyPrint=true`
for multi-line console output. Leave it off for collectors that parse
one event per stdout line. To export structured log records independently
of console formatting, set `RIVER_OTEL_LOGS_ENABLED=true` or JSON
`logging.otlpLogs=true` and configure the standard
`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` for the OTLP gRPC collector. River
batches records in the SDK. Pretty formatting applies only to the local
console stream.
`RIVER_RESOURCE_DISPLAY` separately controls the startup resource chart.

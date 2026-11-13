# Local River stack

`compose.yaml` remains Compose project **river**. Generate it with
`python tools/river-compose.py generate`; infrastructure definitions live in
`deploy/compose.infrastructure.yaml`. The previous Compose file and retired
container state are preserved privately under `reports/container-consolidation`.

The maximum is six existing backend/test containers, including stopped ones:

- `river-stable`: Stable, API http://127.0.0.1:18081
- `river-lts`: LTS, API http://127.0.0.1:18082
- `replica-1`: current experiment, API http://127.0.0.1:18080
- `replica-2`: experiment, API http://127.0.0.1:18083
- `replica-3`: experiment, API http://127.0.0.1:18084
- `replica-4`: experiment/helper, API http://127.0.0.1:18085

The viewer is http://127.0.0.1:18765 and defaults to replica-1. Its existing API
URL form can switch an individual browser to any direct port above. The proxy
sets `X-River-Backend` on responses so the selected backend is observable.

## Start and switch

Run commands from `streaming/river`:

```powershell
python tools/river-compose.py up stable lts replica-1 replica-2 replica-3 replica-4
python tools/river-compose.py audit
python tools/river-compose.py target replica-2 --runtime
python tools/river-compose.py target replica-1 --runtime
```

`target --runtime` reloads Nginx without restarting backend containers. Refresh
an open viewer to reconnect its WebSocket. The selected target is saved locally.
Without `--runtime`, `target` also recreates the old/new backend slots to apply
the default telemetry producer selection. Their configuration/cache survives.

Compose also exposes `RIVER_VIEWER_BACKEND` (for example `stable:8080`), per-slot
`RIVER_REPLICA_1_IMAGE`, `RIVER_REPLICA_1_PORT`, `RIVER_REPLICA_1_CPUS`,
`RIVER_REPLICA_1_MEMORY` and corresponding stable/LTS/replica2–4 variables.
Default backend limits are4CPU/2GiB per slot. Increase only the benchmark slots
when a matched test requires it; this is not a qualified performance preset.

Only the selected target emits OTLP by default, with the existing service name
`river-server` and collector address `http://otel-collector:4317`. Instrumentation
services/configuration are preserved. Per-slot `RIVER_REPLICA_1_OTEL_ENABLED`
and corresponding variables override this. Runtime viewer switching alone does
not change a running backend's telemetry settings; apply Compose for that.

## Reuse slots for tests

```powershell
python tools/river-compose.py slot replica-1 --backend --image river:latest --cpus 16 --memory 8g --env RIVER_OTEL_ENABLED=false
python tools/river-compose.py slot replica-2 --backend --image river:stable --cpus 16 --memory 8g --env RIVER_OTEL_ENABLED=false
python tools/river-compose.py slot replica-4 --origin reports/kakadu-io-m303/org-network-m780/serve.py --manifest /data/reports/fixed-gate-v2/manifest.json --delay-ms 14.5 --rate 300000000
```

An origin occupies replica-4, not an additional container. It serves internally
on8000 and through the slot's host port18085. Existing fixture volumes attach
read-only. `--lab --image river-reader-lab:marked-packed-m212` instead makes a
slot a persistent compiler/helper; `/work` is its persistent report directory.
Switch it back with `slot replica-4 --backend --image river:latest`.

The helper stops/removes a slot **before** Compose up, avoiding a transient
seventh backend. It never removes volumes. Do not use ad hoc Docker run/create,
Compose run, or historical report scripts that create per-experiment containers.
Mutating tests belong in replicas; permanent Stable/LTS instances stay protected.
Matched timing must explicitly match CPU/memory and disable telemetry on both
comparison replicas. Use Compose exec for commands inside an existing slot.
The old `verify-bundle-upgrade.py` runner is disabled because it creates foreign
Compose projects. Upgrade checks must stage baseline/candidate images sequentially
in one replica and preserve its configuration/cache, within these same six slots.

Private state is in ignored `deploy/local-stack.json` and
`deploy/local-state/<slot>/config`. Each slot has persistent cache storage;
replica-1 retains the legacy `river_river-s3-cache` volume. Corpus/data volumes,
storage services and existing instrumentation are preserved. Never run down -v.

CORS accepts any browser hosting origin by default, including credentialed
requests and custom headers. Set global `RIVER_CORS_ORIGINS` or override an
individual slot with `RIVER_<SLOT>_CORS_ORIGINS` to restrict origins explicitly.
See `deploy/portable/agent-handoff/STREAMER-COMPATIBILITY.md` for the shared
tile-info/default256pixel pyramid contract; deployed availability follows releases.json.

Local Compose and portable deployment default to 16 concurrent reads per
source. An explicit RIVER_S3_SOURCE_MAX_REQUESTS value overrides the Compose
default. A saved read-policy maxSourceRequests value takes precedence over
both. The cap controls source connections separately from automatic source-I/O
and decode sizing.

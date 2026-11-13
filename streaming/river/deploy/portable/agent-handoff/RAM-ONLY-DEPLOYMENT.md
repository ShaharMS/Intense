# RAM-only cache and decoder scratch

Linux River chooses RAM-only mode when the configured `RIVER_S3_CACHE_ROOT`
is outside a writable mounted cache volume. A writable container overlay
does not qualify. A mounted cache volume keeps the existing persistent cache
behavior unless `no-disk` is set. River keeps its process-local caches and a
bounded in-memory asset locator in RAM-only mode. Files from decoder fallback
are temporary scratch. They are released after the request.

| Setting | Default and meaning |
| --- | --- |
| `RIVER_AUTO_ADAPTATION_MODE` | Add `no-disk` to force RAM-only cache and decoder scratch, even with an attached cache volume. A comma-separated list such as `liberal,no-disk` works for the storage constraint. |
| `RIVER_RAM_SCRATCH_ROOT` | `/dev/shm`. A custom directory must already exist on a writable tmpfs mount. |
| `RIVER_RAM_SCRATCH_BYTES` | One eighth of effective River memory by default. A value of zero disables file-based scratch. Explicit values are capped at half the effective memory limit. |
| `RIVER_RESOURCE_DISPLAY` | `auto`. Select `color`, `unicode`, `ascii`, or `off` to override startup display. Auto uses ASCII for redirected logs and honors NO_COLOR on terminals. |

Settings are read at startup. `/api/diagnostics/resources` reports the
storage decision, verified scratch path, configured scratch allowance and
active scratch reservations. The actual tmpfs mount also has its own
capacity. Both limits apply. A request that cannot reserve space receives
`503 decoder_ram_scratch_unavailable`. There is no disk spill.

For Compose, use the included `compose.ram-only.yaml` after the base file to
remove the cache mount and set a read-only root. It retains the existing
configuration and source data mounts. Add the TLS overlay last if needed.

    docker compose -f compose.yaml -f compose.ram-only.yaml up -d

For Kubernetes, omit the cache PVC. For compatibility decodes that exceed
the default /dev/shm capacity, mount a memory-backed `emptyDir` with an
explicit sizeLimit, and set `RIVER_RAM_SCRATCH_ROOT` to that mount. Size the
mount and the River limit together. This workspace counts against the pod
memory limit. Preserve existing configuration, source mounts and secrets.

If using the nvJPEG2000 file-based sidecar, mount a dedicated tmpfs scratch
directory visible to both processes and include its path in the existing
`RIVER_NVJPEG2000_ALLOWED_ROOTS` allowlist. The release's diskless pixel
checks cover OpenJPEG and Grok CLI.

The no-disk guarantee covers River cache and decode scratch writes.
Configuration persistence, the container log driver and host swap remain
deployment choices. Cached asset locators are process-local in RAM-only
mode and can be repopulated from source URLs after restart.

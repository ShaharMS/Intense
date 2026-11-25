# River deployment

Read UPGRADE-FROM-M1232.md before upgrading an existing deployment. This package
contains an exported Linux amd64 image, Compose templates, its OpenAPI schema,
release record and checksums. Docker/Compose are prerequisites; Podman with
a Compose provider is also supported by the scripts where explicitly selected.

## Upgrade an existing OpenShift deployment

Extract the ZIP and run the commands below in the extracted directory. Replace the example registry, namespace, deployment and container names with the ones used by your installation. The workstation needs Podman, registry push access and an authenticated OpenShift CLI. The cluster needs pull access to the chosen repository.

```sh
sha256sum -c SHA256SUMS
podman load -i river-image.tar.gz
podman image inspect localhost/river:m1233 --format '{{.Id}}'
```

Match the printed image ID to BUILD-MANIFEST.json. Push the versioned image and use the manifest digest returned by your registry for the rollout. The registry manifest digest and the local image ID identify different objects.

```sh
repo=registry.example.org/imagery/river
namespace=imagery
deployment=river
container=river
podman tag localhost/river:m1233 "$repo:m1233"
podman push --digestfile m1233.registry-digest "$repo:m1233"
oc -n "$namespace" rollout history "deployment/$deployment"
# Save the current revision number and image reference for rollback.
oc -n "$namespace" get deployment "$deployment" -o jsonpath='{.metadata.annotations.deployment\.kubernetes\.io/revision}' > previous.revision
oc -n "$namespace" get deployment "$deployment" -o jsonpath='{range .spec.template.spec.containers[*]}{.name}{" "}{.image}{"\n"}{end}' > previous-images.txt
oc -n "$namespace" set image "deployment/$deployment" "$container=$repo@$(cat m1233.registry-digest)"
oc -n "$namespace" rollout status "deployment/$deployment"
```

For a GitOps-managed installation, set that same image digest in the existing workload manifest and sync it. Keep the current source IDs, Secrets, environment, resource limits, service and routes. For RAM-only operation retain `RIVER_AUTO_ADAPTATION_MODE=liberal,no-disk`. Scratch and cache sizes can stay automatic. Cache learning remains supported.

OpenJPEG and native Grok can decode compatible S3 inputs directly from process memory, including whole-object fallback within the configured budget. These paths work without a custom `/dev/shm` mount. A file-only path that creates named scratch files still needs writable tmpfs in RAM-only mode. An existing memory-backed mount can stay in place. Both process memory and tmpfs use count toward the pod limit. Inspect `/api/diagnostics/resources` for the effective storage mode and budget.

Verify `/health`, version `m1233` at `/openapi.json`, and RAM-only storage at `/api/diagnostics/resources`. Request a known WMTS tile through your existing route. To read the expanded diagnostics, use the existing debug setting or set `RIVER_LOG_LEVEL=debug` in the workload configuration. A timed WMTS request can then be diagnosed from the pod logs alone.

For a directly managed Deployment with the recorded revision still available, roll back using the command below. For a GitOps deployment, revert the workload image digest in the tracked manifest and sync it.

```sh
oc -n "$namespace" rollout undo "deployment/$deployment" --to-revision="$(cat previous.revision)"
oc -n "$namespace" rollout status "deployment/$deployment"
```

## Fresh local Compose installation

For a fresh installation, verify SHA256SUMS, copy `.env.example` to `.env`, set a
private administrator token, and run `sh start.sh` or `./start.ps1`. The default
API is http://localhost:8080 and Swagger is at `/docs/`. Configure sources through
ENV or the API. No source listing or registration is required.

ENV sources use one-based numbering: `RIVER_S3_SOURCE_COUNT=1`, then
`RIVER_S3_SOURCE_1_ID`, `_BUCKET`, `_REGION`, `_ACCESS_KEY_ID`, `_SECRET_ACCESS_KEY`,
and optional `_ENDPOINT`, `_PREFIX`, `_ALLOW_HTTP`, `_VIRTUAL_HOSTED_STYLE` and
`_SESSION_TOKEN`. Set `RIVER_ENABLED_DRIVERS=openjpeg,jp2grok,openhtj2k,nvjpeg2000`
only for the drivers your deployment intends to permit; inspect availability.

The default Compose file attaches configuration and read-only corpus directories,
not a cache volume. Caches and bounded decoder scratch use RAM. Set container CPU
and memory limits in your orchestration system; River sizes against those limits.
`shm_size` must support expected decoder scratch demand and remains charged to the
container's memory. An attached persistent cache can be enabled with
`docker compose -p river -f compose.yaml -f compose.cache.yaml up -d`.
`no-disk` is a hard adaptation override even if a cache volume is attached.

For GPU use, configure the NVIDIA container runtime and include compose.gpu.yaml.
For TLS, mount PEM certificate/key or PFX under certificates and set the existing
`RIVER_TLS_ENABLED=true`, `RIVER_TLS_CERT_FILE` and `RIVER_TLS_KEY_FILE`,
or `RIVER_TLS_PFX_FILE` and optional `RIVER_TLS_PFX_PASSWORD_FILE`. Select protocols
with `RIVER_TLS_PROTOCOLS=http/1.1,h2,h3`; `RIVER_TLS_BIND` sets the TLS listener. HTTP/3 needs the published UDP port as well as a trusted certificate.
Consult the bundled OpenAPI configuration schema for exact protocol/TLS settings.
An empty certificates directory does not enable TLS.

The m1233 bundle assigns one digest to all three channels. Its release record documents that the release benchmark suite was skipped for this packaging-only release.

On SELinux hosts, authorize bind mounts using shared `:z` labels (for example
`./certificates:/run/river-tls:ro,z`) or the equivalent platform policy. Keep private
keys readable only by the service identity; do not disable TLS verification or
make production keys world-readable to work around a mount-label denial.

## WMTS response diagnostics

With `RIVER_LOG_LEVEL=debug` or `trace`, WMTS tile responses include the work performed inside preparation and rendering in `Server-Timing`. For example, `source_index` measures locating tile parts, `source_payload` measures the native payload prefetch, and `prepare_memory_admission` measures the memory reservation. `X-River-Source-Stages` attributes range GETs and source bytes to the same source stages. `X-River-Index-Source` identifies the offset lookup used, such as `cold-sot` for discovery from tile-part headers or `tlm` for the embedded table. Read the inner stages as a breakdown. The metrics `prepare`, `render`, and `source_total` are inclusive, and decoder work can overlap preparation. These headers work with request-summary logging disabled.

A tile with `X-River-Decoder-Route: s3-shared-native-view` borrowed an already assembled compressed input for the same source version and native JPEG 2000 tile. Different WMTS coordinates can share it. The range-cache budget covers these inputs, and active decoder leases survive cache eviction. In this route, cache-hit bytes describe the size of the borrowed input. The decoder can still spend time producing pixels for the requested coordinate.

For full-resolution 512-pixel WMTS tiles, automatic selection can use native Grok on lossless 8-bit RGB LRCP imagery with 10–12 resolution levels and square 1024-, 2048-, or 4096-pixel native tiles. This choice requires the native bridge and a configured pool of at least eight threads. An explicit WMTS driver selection still takes precedence.

### Large S3 images and the compatibility limit

A WMTS 422 mentioning the compatibility limit of `2147483648` bytes means the selective reader failed and the complete source object exceeds the configured fallback size. The preceding cause in the response identifies why River attempted the fallback. Selective range reads can serve objects larger than this limit.

| Environment variable | Default | What it limits |
| --- | --- | --- |
| `RIVER_S3_COMPATIBILITY_BYTES` | 2147483648 (2 GiB) | Maximum size of a complete source object River may materialize for compatibility decoding. |
| `RIVER_S3_REQUEST_BYTES` | 64 MiB normally, up to 128 MiB automatically for complete native parts in oversized sources | Compressed source bytes for selective request preparation. An explicit setting also overrides automatic read-budget adjustments. |

When a complete native tile part is required and the source exceeds the compatibility ceiling, River can use the compressed-input memory reserved for its source-I/O lane. With automatic settings this allowance rises from 64 MiB to 128 MiB as available memory per lane increases. To allow 128 MiB for selective requests explicitly, set `RIVER_S3_REQUEST_BYTES=134217728`.

Both settings accept a positive integer byte count. For example, to allow whole-object fallback for objects up to 4 GiB, add the following entry to the River container's OpenShift `env` list.

```yaml
- name: RIVER_S3_COMPATIBILITY_BYTES
  value: "4294967296"
```

This setting applies to the River process. Whole-object fallback can transfer the entire object for one tile and needs space for that input. In RAM-only deployments it also consumes the pod's memory and requires sufficient scratch allowance. Increasing the compatibility-cache retention size does not raise the object-size ceiling.

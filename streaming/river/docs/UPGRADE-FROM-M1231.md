# Upgrade from m1231 to m1232

River m1232 is a Linux amd64 image. The archive carries four tags for the same image: `m1232`, `lts`, `stable` and `latest`.

## What changed

- **RAM-only deployments.** Liberal mode retains more cache while keeping the runtime, temporary and decoder reserves at the 32 GiB boundary. In a 16 CPU, 32 GiB automatic plan the shared cache target moves from 22.38 to 17.98 GiB. Admission also accounts for pressure in visible constrained cgroup ancestors. The range cache charges per-entry overhead. At 92% effective usage, a half-second pressure check can reduce optional cache targets to 10% and reclaim decoder coefficients and preview checkpoints. Active readers can still hold memory until their work finishes.
- **Failure diagnostics.** The response request ID correlates with a pod log record. Preparation and render errors include the stage, WMTS replay parameters, selected driver and available native error chain. Debug adds resident source structure, per-component precision and sampling, JP2 channel definitions, source reads, cache counters, worker budgets and timings when available. Header failures can report the violated constraint before a tile plan exists. Information from native codecs is bounded.
- **Display pixels.** River follows declared JP2 colour and opacity associations through channel reordering, including BGR with an auxiliary band, alpha-first RGB, grayscale with auxiliary bands, and premultiplied opacity. It accepts four-component sRGB with three distinct, explicit RGB associations and an omitted auxiliary entry. Grayscale fixtures matching older and newer GDAL JP2 writers are covered. Persistent encoded tiles use a new cache version; expect cold tile misses after upgrading.
- **Unlabelled imagery.** By default, one or two bands display band 1 as opaque gray. Three or four bands use the first three as RGB unless the source declares grayscale. The fourth band has no inferred wavelength. For an unlabelled four-band collection, `RIVER_UNDEFINED_FOUR_BAND_LAYOUT` accepts `rgb` (default), `bgr`, `rgba`, `bgra` or `reject`. Explicit labels take precedence.
- **Strict ingestion.** Set `RIVER_REQUIRE_CHANNEL_LABELS=true` to require an explicit JP2 definition for every channel. This rejects raw codestreams and otherwise renderable images with an undefined auxiliary band. Leave this setting unset or false for normal ingestion. Conflicting four-band colour assignments still produce a 422 diagnostic. Multiband ICC/palette transforms and component precision above 16 bits remain unsupported.

## OpenShift

1. Extract the ZIP. From the extracted directory, run `sha256sum -c SHA256SUMS` and `podman load -i river-image.tar.gz`.
2. Follow the push-by-digest and rollback commands in [DEPLOYMENT.md](DEPLOYMENT.md). Record the current image digest or GitOps revision before changing the River container image.
3. Existing m1231 deployments can keep `RIVER_AUTO_ADAPTATION_MODE=liberal,no-disk` and `RIVER_CACHE_LEARNING=true`. Automatic scratch and cache sizing remain supported. Both new channel settings are optional.
4. Wait for rollout, check `/health`, and confirm `/openapi.json` reports `m1232`. For RAM budgeting, inspect `/api/diagnostics/resources`. For a failed tile, use its response request ID in the pod log. Restore the previous registry digest or GitOps revision to roll back.

See [SETTINGS.md](SETTINGS.md) for the display settings and failure record fields. The package includes its frozen source manifest, image ID and checksums. The packaged release skipped the full benchmark suite.

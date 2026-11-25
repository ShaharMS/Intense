# Upgrade from m1230 to m1231

River m1231 ships one Linux amd64 image. The archive loads the same image under `localhost/river:m1231`, `localhost/river:lts`, `localhost/river:stable` and `localhost/river:latest`. BUILD-MANIFEST.json contains its image ID. QUALIFICATION.json records the packaging-only release and the skipped image tests.

## What changed

- **Cold, reduced-resolution WMTS tiles.** On the `s3-multipart-partial-compact-prefix` route, JPEG 2000 files can require a chain of small reads to locate inline packet headers. After an earlier tile part demonstrates this layout, River can request the next eligible part alongside the parts needed by other selected tiles. Packet validation and the requested quality and resolution are preserved. Existing imagery can be used as-is.
- **Read limits.** Read-ahead applies to eligible parts larger than 16 KiB and at most 256 KiB, subject to the request's remaining byte and in-memory lease budgets. It can read past the last useful packet inside a selected part. An explicit per-request byte or range cap keeps the narrower read pattern. A usable per-part packet-length table can already identify the needed packet boundaries.
- **Debugging from pod logs.** At `RIVER_LOG_LEVEL=debug`, a timed tile emits a diagnostic even when `RIVER_LOG_REQUEST_SUMMARY=false`. The record includes coordinates, decoder route, image and native tile geometry, packet-table coverage, inline probe counts, origin GETs and bytes, and times for header reads, parsing, payload fetch and assembly. `prefetched_inline_parts` counts parts included in the wider read wave. Verbose summaries print a per-stage I/O table. Parent stage times include their children. SETTINGS.md explains the fields.

## OpenShift upgrade

Use DEPLOYMENT.md for the commands to verify the ZIP contents, load the image, push the versioned tag and update the existing Deployment by registry digest. Record the current deployed digest or GitOps revision first. For rollback, restore it and wait for rollout completion.

Keep existing Secrets, source definitions, routes and resource limits. Existing `RIVER_AUTO_ADAPTATION_MODE=liberal,no-disk` and `RIVER_CACHE_LEARNING=true` deployments can keep both settings. Scratch and cache sizes can stay automatic. The release requires no new environment variable, volume or image re-encoding. To see detailed tile diagnostics, set `RIVER_LOG_LEVEL=debug` in the existing configuration and request a tile.

After rollout, check `/health` and verify that `/openapi.json` reports m1231. For a matching slow tile, inspect the latest pod log for `s3-multipart-partial-compact-prefix`, `prefetched_inline_parts` and `source_prefix_probe_read`. A positive prefetch count and fewer origin GETs in the probe stage show where the change helped.

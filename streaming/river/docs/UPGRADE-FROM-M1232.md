# Upgrade from m1232 to m1233

River m1233 ships one Linux amd64 image under the tags m1233, lts, stable and latest.

## What changed

River and its bundled OpenJPEG decoder now accept sparse JP2 channel definitions on 4–16 component sRGB and grayscale sources when every display colour is explicitly assigned. Any subset of auxiliary bands can be omitted. River honours declared whole-image opacity, including premultiplied opacity. It renders opaque tiles when the source declares no opacity.

For the reported eight-band WorldView-3 layout, the JP2 identifies bands 5, 3 and 2 as red, green and blue. m1232 rejected its header before rendering any tile. m1233 follows the file's assignments, including other band orders. Missing display colours, duplicate assignments and out-of-range channels still produce diagnostics. Grok requests can use OpenJPEG fallback for these layouts.

## OpenShift upgrade

1. Extract the ZIP and run `sha256sum -c SHA256SUMS` in the extracted directory.
2. Load the image with `podman load -i river-image.tar.gz`. Follow [DEPLOYMENT.md](DEPLOYMENT.md) to push it to your registry, retain the previous digest for rollback and roll the deployment to the new digest.
3. Keep existing cache, scratch, cache-learning and liberal/no-disk settings. For sources with unlabelled auxiliary channels, leave `RIVER_REQUIRE_CHANNEL_LABELS` unset or false. Strict mode requires every channel to be labelled.
4. Verify `/health`, version `m1233` at `/openapi.json`, and a previously failing tile. Existing WMTS URLs work without edits or source re-encoding.

Mapping comes from embedded JP2 metadata. Collection-level custom mappings and catalog lookup are proposals for a future release. Qualification here uses generated JP2 fixtures and a packaged-image smoke check. The consumer's original file and production OpenShift environment were unavailable.

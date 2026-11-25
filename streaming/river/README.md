# River

River serves JPEG 2000 imagery directly from configured sources through WMTS and HTTP tile requests. Swagger is available at `/docs/`.
There is no viewer, image catalog, bucket browser or registration requirement.

- [Deployment](docs/DEPLOYMENT.md)
- [Changes from m1227](docs/UPGRADE-FROM-M1227.md)
- [Cache behavior and learning](docs/CACHE-GUIDE.md)
- [Release qualification](deploy/releases.json)

## Layout

`crates/river-core` owns the public protocol and models. `crates/river-server/src`
contains the rendering engine and source planner, with `caching/`, `runtime/`,
`hosting/`, `settings/`, `decoder/` and `remote_source/` implementation areas.
Native codec dependencies remain in their dedicated crates and keep their licenses.

`tools/fixtures` generates and inspects imagery; `tools/benchmarks` contains workload
comparisons; `tools/checks` contains verification utilities. Top-level tool entrypoints
build, package and operate releases. `deploy/` contains maintained templates and
private local deployment state. `evidence/` contains retained qualification records.

All generated output is under Cargo's `target/`. Source imagery is flat under
`corpus/`; generated fixture metadata lives under `target/fixtures`, not alongside
image files. Local settings and data volumes are not disposable build output.

## Local corpus

Install Pillow with OpenJPEG support, then run:

```sh
python3 tools/fixtures/generate-corpus.py
```

This generates three reproducible 4096×4096 RGB source images and five lossless
JPEG 2000 layouts per source: plain, TLM, TLM+PLT, multilayer, and single-tile,
covering four through six wavelet levels. Generation verifies decoded pixels.
No image download is needed. The synthetic set occupies roughly 0.9 GB.

## Build and deployment

```sh
python3 tools/build-release.py VERSION --engine docker
python3 tools/river-compose.py generate
python3 tools/river-compose.py up replica-1
```

A build is a candidate until qualification names its exact image digest. The
packager refuses evidence for a different image:

```sh
python3 tools/package-deployment.py VERSION --image river:VERSION \
  --evidence target/benchmarks/qualification.json --engine docker
```

Set `RIVER_CONTAINER_ENGINE=podman` when using Podman with a Compose provider.
Use the existing `river` project and at most six backend slots: `river-stable`,
`river-lts`, and `replica-1` through `replica-4`. Stable/LTS/Latest are aliases for
one qualified implementation, not independent source branches.

Historical scripts may require fixtures/evidence from their original experiments;
the generated corpus and consolidation harness are the current reproducible path.

## Map service

River Atlas serves geographic raster tiles from the existing JPEG 2000 pipeline. See the [map service guide](docs/MAPS.md) for imagery preparation and the globe viewer.

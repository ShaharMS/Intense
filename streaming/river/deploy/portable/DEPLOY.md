# River portable deployment

This package contains exactly two Linux AMD64 images: the complete backend and
the complete UI. No source checkout, compiler, Python installation, package
download, external CDN, database, or separate NVIDIA sidecar is needed at run
time. The backend contains OpenJPEG, JP2Grok v20.4.15, NVIDIA nvJPEG2000
0.11.0.51, the CUDA 13.3 runtime, and WebP/PNG/AVIF/JPEG XL encoders. The private
NVIDIA helper is supervised inside the backend and listens only on loopback.

## Start on another PC

Install Docker with Linux-container support and Docker Compose v2. Copy this
whole directory. On Windows use Docker Desktop with WSL2:

```powershell
./start.ps1           # CPU: OpenJPEG and JP2Grok
./start.ps1 -Gpu      # Also enables nvJPEG2000 on an NVIDIA GPU
```

On Linux:

```sh
sh start.sh           # CPU
sh start.sh --gpu     # NVIDIA
```

The scripts verify and load both archives, copy `.env.example` if `.env` is absent, and wait for container health. Images can
also be loaded manually with `docker load --input <archive>`.

NVIDIA mode requires a compatible NVIDIA GPU, a host driver supporting the
bundled CUDA 13.x runtime, and Docker GPU access. Linux needs the NVIDIA
Container Toolkit; Windows needs WSL2 GPU support. Host CUDA toolkit and
Python installations are not needed. CPU mode keeps the NVIDIA software in
the image but reports that driver unavailable. Check `/api/drivers` rather
than assuming GPU availability. ARM PCs need AMD64 emulation for CPU mode;
this build is not a native ARM or macOS GPU image.

Default addresses:

- UI: `http://127.0.0.1:18765/`
- API and Swagger: `http://127.0.0.1:18080/docs/`
- OpenAPI: `http://127.0.0.1:18080/openapi.json`
- Swagger through the UI proxy: `http://127.0.0.1:18765/docs/`

Change ports or bind address in `.env`. To serve other PCs on your LAN, set
`RIVER_BIND_ADDRESS` to the server's LAN address or `0.0.0.0`, and open the UI
port in the host firewall. The UI always uses its own origin and proxies to
the backend, so no hardcoded localhost API address needs changing. Catalog
and tile reads are public; configuration APIs require no token.

## Imagery and cache

One SHA-verified 5 MiB test image is bundled for offline startup and validation.
The large imagery corpus and prepared caches are deliberately outside the
images. Copy your imagery and `river-assets.json` into `data/`, preserving the
relative paths used by the catalog, or set `RIVER_DATA_DIR` in `.env`. Data is
mounted read-only. A minimal catalog is:

```json
{"assets":[{"assetId":"<original file SHA-256>","source":"image.jp2","width":40980,"height":10980,"bitDepth":16,"nativeTileWidth":1024,"nativeTileHeight":1024}]}
```

Restart the backend after replacing the catalog. Images can also be registered
from S3/MinIO using Swagger and the existing UI settings. Runtime S3 connection
credentials are held in memory and must be reconfigured after restart.

The named `river-cache` volume persists original byte copies and lossless
HTJ2K compressed derivatives; original imagery is never edited. Default quotas
are 2 GiB for original byte copies and 1 GiB for admitted HTJ2K payloads. Set
`RIVER_HT_CACHE_BYTES=0` to disable HT admission/lookup. Temporary preparation
files and decoder working memory are outside the admitted-payload quota.
Warm-cache performance depends on cache hit rate, host CPU/GPU and storage.
Worker counts and decode concurrency are configurable in `.env`; the 32-worker
setting was measured on an RTX 4060 laptop with 8 GiB VRAM.

The viewer retains trace history for the browser page session without another
service. For distributed trace export, set `RIVER_OTEL_ENABLED=true` and point
`OTEL_EXPORTER_OTLP_ENDPOINT` at your existing OTLP/gRPC collector. No collector
is required for standalone startup. WebSocket message/framing details are
documented under the Streaming section in Swagger.

## Stop

```sh
docker compose -p river-portable -f compose.yaml down
```

Do not add `--volumes` if you want to retain the compressed caches. The image
archives, sizes, image IDs and SHA-256 hashes are recorded in `BUILD-MANIFEST.json`
and `SHA256SUMS`. Component license notices are included in the backend image
under `/usr/share/doc/river/licenses` and in this package's `licenses/` directory.

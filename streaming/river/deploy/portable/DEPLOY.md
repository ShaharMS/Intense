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

## Browser origins and reverse proxies

CORS works from any browser origin by default, including credentialed clients,
custom headers, tile images and readable error responses. `RIVER_CORS_ORIGINS=*`
is the default; use a comma-separated origin list only when restrictions are
intentional. Existing installations with an old generated localhost-only value
must change that value to `*` to enable arbitrary hosting origins.

Keep the browser page and API on compatible protocols: an HTTPS page needs an
HTTPS/WSS API behind your existing TLS proxy. Forward River's CORS headers;
do not add a second Access-Control-Allow-Origin header in the proxy.

### Environment and live settings

The viewer fetches one S3 provider page per action. Choose 25, 50 or 100 keys
per catalog page and press Search or Enter. Prefix mode sends the prefix to
S3. Contains mode filters the returned page locally on the server and can
show zero matches with more pages available. Use Next page to continue.

At startup River applies JSON configuration first, then container environment
variables. A numbered ENV source overrides the complete JSON connection with
the same ID. Other JSON connections are preserved. Send source changes through
POST /api/s3/sources (the same ID replaces the active connection).

Example container environment:

    RIVER_S3_SOURCE_COUNT=1
    RIVER_S3_SOURCE_1_ID=imagery
    RIVER_S3_SOURCE_1_BUCKET=imagery
    RIVER_S3_SOURCE_1_REGION=us-east-1
    RIVER_ENABLED_DRIVERS=openjpeg,jp2grok

Supply RIVER_S3_SOURCE_1_ACCESS_KEY_ID and RIVER_S3_SOURCE_1_SECRET_ACCESS_KEY
from your secret store. Optional per-source fields are NAME, ENDPOINT, REGION,
PREFIX, SESSION_TOKEN, ALLOW_HTTP, VIRTUAL_HOSTED_STYLE, and MAX_RETRIES.
Boolean fields accept true or false. Pass the variables through the container
environment or the service's Compose env_file.

JSON accepts an optional enabledDrivers list. Read or update the active list
at GET or PUT /api/settings/drivers with a JSON body such as
{"enabledDrivers":["openjpeg"]}. Null allows every available decoder.
Live API changes take precedence until restart. Drivers still require their
runtime dependencies. Explicit requests for a disabled driver are rejected.

Environment values are reapplied on restart. Set RIVER_CONFIG_MODE=env to
skip JSON entirely and keep API edits in memory. In JSON mode, unrelated
API edits do not copy environment-supplied sources into the file.

### Native TLS and protocol selection

Mount the certificate material read-only. River accepts a PEM chain and key,
or a PFX/PKCS#12 identity. Use one of these sets of container variables:

    RIVER_TLS_CERT_FILE=/run/river-tls/tls.crt
    RIVER_TLS_KEY_FILE=/run/river-tls/tls.key

    RIVER_TLS_PFX_FILE=/run/river-tls/river.pfx
    RIVER_TLS_PFX_PASSWORD_FILE=/run/river-tls/password

The password file is optional for a PFX with an empty password. Never place
the private key or password in JSON. The optional JSON tls object accepts
bind, certFile, keyFile, pfxFile, pfxPasswordFile, protocols and http3PublicPort.
ENV overrides JSON. TLS settings and certificate rotation require a restart.
RIVER_TLS_ENABLED=false disables native TLS.

RIVER_TLS_PROTOCOLS accepts a comma-separated list of http/1.1, h2 and h3.
The default is http/1.1,h2. JSON uses an array such as ["h2", "h3"].
RIVER_TLS_BIND defaults to 0.0.0.0:8443. HTTP/1.1 and HTTP/2 use TCP.
HTTP/3 uses UDP on the same listener port. RIVER_HTTP3_PUBLIC_PORT sets the
advertised UDP port when it differs from the bind port. WebSockets use HTTPS
over HTTP/1.1 or HTTP/2. Keep one of those protocols for the progressive viewer.

Use a Kubernetes TLS Secret mounted at /run/river-tls for PEM, or a generic
Secret with the PFX and password files. Target TCP and/or UDP port 8443 in
the Service to match the selected protocols. The original plaintext port
remains available for internal traffic and health probes. Publish only the
TLS port for an HTTPS-only public endpoint.

### Request logging

At startup, use the optional JSON logging object or these container variables.
Environment values override JSON. Restart River after changing these settings.

    RIVER_LOG_LEVEL=info
    RIVER_LOG_REQUEST_SUMMARY=true
    RIVER_LOG_VERBOSE=false
    RIVER_LOG_UNPROCESSABLE_LEVEL=warn

The corresponding JSON fields are minimumLevel, requestSummary, verbose and
unprocessableLevel. Levels are off, error, warn, info, debug and trace.
Without an explicit logging configuration, RUST_LOG still accepts advanced
target filters. For detailed diagnostics, set verbose to true and the level to
debug. The HTTP summary uses readable field names and includes the status,
safe route template, request ID, trace ID, timings, selected driver, actual
decoder backend and source IO counters where the response supplies them.

Collect River's stdout with your platform's log agent. An OpenTelemetry
Collector or Grafana Alloy can read the container logs and fan them out to
Loki or other configured sinks. Configure bounded queues and retries in the
collector. Keep sink credentials and network delivery in that collector.

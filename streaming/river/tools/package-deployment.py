"""Package an already exported combined API/UI milestone for offline deployment."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import zipfile
from deployment_handoff import install_handoff

parser = argparse.ArgumentParser()
parser.add_argument('milestone')
parser.add_argument('--suffix', default='', choices=['','-final','-native-pool','-16core-guide','-qualified','-serial-auto'])
parser.add_argument('--performance-report', type=Path)
parser.add_argument('--configuration-profile', type=Path, help='Attach a qualified, image-specific opt-in configuration profile.')
parser.add_argument('--stage-only',action='store_true',help='Prepare the deployment folder for upgrade verification before final ZIP assembly')
parser.add_argument('--enable-structural-reads', action='store_true', help='Enable verified precinct/multipart paths in this candidate bundle')
parser.add_argument('--enable-single-prefix', action='store_true', help='Enable verified single-native-tile coarse projections in this candidate bundle')
parser.add_argument('--enable-coarse-worker', action='store_true', help='Enable the verified reversible coarse-stream OpenHT worker path')
parser.add_argument('--enable-exact-support', action='store_true', help='Enable verified per-band reversible wavelet support selection')
parser.add_argument('--enable-streamed-plt-roi', action='store_true', help='Enable bounded spatial reads for large single-tile PLT tables')
parser.add_argument('--enable-index-only-prefix', action='store_true', help='Enable bounded prefix reads when PLT indexes precede packet data in empty tile-parts')
args = parser.parse_args()
structural_default = 'true' if args.enable_structural_reads else 'false'
single_prefix_default = 'true' if args.enable_single_prefix else 'false'
coarse_worker_default = 'true' if args.enable_coarse_worker else 'false'
exact_support_default = 'true' if args.enable_exact_support else 'false'
streamed_roi_default = 'true' if args.enable_streamed_plt_roi else 'false'
index_only_default = 'true' if args.enable_index_only_prefix else 'false'
root = Path(__file__).resolve().parents[1]
build = root / 'reports/image-builds' / args.milestone
status = json.loads((build / 'status.json').read_text(encoding='utf-8-sig'))
assert status['state'] == 'built'
configuration_profile = None
serial_default = 'sdk'
if args.configuration_profile:
    configuration_profile = json.loads(args.configuration_profile.read_text(encoding='utf-8'))
    assert configuration_profile['qualifiedForUse'] is True and configuration_profile['imageId'] == status['imageId']
    assert configuration_profile['overrides'] == {'serialTransport': 'auto'}
    serial_default = 'auto'
image = Path(status['archive'])
assert hashlib.sha256(image.read_bytes()).hexdigest() == status['archiveSha256']
package = root / 'dist' / ('river-deploy-' + args.milestone + args.suffix)
package.mkdir(exist_ok=False)
shutil.copyfile(image, package / 'river-image.tar.gz')
tag = status['tag']
(package / 'compose.yaml').write_text('''name: river-portable
services:
  river:
    image: ''' + tag + '''
    pull_policy: never
    restart: unless-stopped
    environment:
      RIVER_S3_ADAPTIVE_READS: ${RIVER_S3_ADAPTIVE_READS:-observe}
      RIVER_S3_SOURCE_MAX_REQUESTS: ${RIVER_S3_SOURCE_MAX_REQUESTS:-16}
      RIVER_S3_MAX_RETRIES: ${RIVER_S3_MAX_RETRIES:-2}
      RIVER_S3_PARSED_HEADER_CACHE_BYTES: ${RIVER_S3_PARSED_HEADER_CACHE_BYTES:-16777216}
      RIVER_S3_PACKET_PLAN_CACHE_BYTES: ${RIVER_S3_PACKET_PLAN_CACHE_BYTES:-67108864}
      RIVER_S3_PREP_METADATA_BYTES: ${RIVER_S3_PREP_METADATA_BYTES:-4194304}
      RIVER_TRACE_SOURCE_READS: ${RIVER_TRACE_SOURCE_READS:-0}
      RIVER_PROFILE_SOURCE:
      RIVER_PROFILE_CODEC:
      RUST_LOG:
      RIVER_S3_SERIAL_TRANSPORT: ${RIVER_S3_SERIAL_TRANSPORT:-''' + serial_default + '''}
      RIVER_S3_PRECINCT_ROI: "${RIVER_S3_PRECINCT_ROI:-''' + structural_default + '''}"
      RIVER_S3_MULTIPART_PLT: "${RIVER_S3_MULTIPART_PLT:-''' + structural_default + '''}"
      RIVER_S3_MULTIPART_PREFIX: "${RIVER_S3_MULTIPART_PREFIX:-''' + structural_default + '''}"
      RIVER_S3_SINGLE_PREFIX: "${RIVER_S3_SINGLE_PREFIX:-''' + single_prefix_default + '''}"
      RIVER_S3_INDEX_ONLY_HEADER_WINDOW_BYTES: "${RIVER_S3_INDEX_ONLY_HEADER_WINDOW_BYTES:-0}"
      RIVER_S3_INDEX_ONLY_PREFIX: "${RIVER_S3_INDEX_ONLY_PREFIX:-''' + index_only_default + '''}"
      RIVER_S3_STREAMED_PLT_ROI: "${RIVER_S3_STREAMED_PLT_ROI:-''' + streamed_roi_default + '''}"
      RIVER_OPENHT_COARSE: "${RIVER_OPENHT_COARSE:-''' + coarse_worker_default + '''}"
      RIVER_S3_EXACT_SUPPORT: "${RIVER_S3_EXACT_SUPPORT:-''' + exact_support_default + '''}"
      RIVER_S3_PRECINCT_REBASE: "${RIVER_S3_PRECINCT_REBASE:-auto}"
      RIVER_PROFILE: ${RIVER_PROFILE:-auto}
      RIVER_DECODE_CONCURRENCY:
      RIVER_SOURCE_IO_CONCURRENCY:
      RIVER_OPENJPEG_THREADS_PER_DECODE: ${RIVER_OPENJPEG_THREADS_PER_DECODE:-auto}
      RIVER_ENABLE_NVIDIA: ${RIVER_ENABLE_NVIDIA:-auto}
      RIVER_NVJPEG2000_NATIVE_WORKERS: "${RIVER_NVJPEG2000_NATIVE_WORKERS:-}"
      RIVER_GROK_NATIVE: ${RIVER_GROK_NATIVE:-true}
      RIVER_GROK_NATIVE_THREADS:
      RIVER_GROK_PACKED_VIEWS: ${RIVER_GROK_PACKED_VIEWS:-true}
      RIVER_OPENHT_WORKERS: ${RIVER_OPENHT_WORKERS:-1}
      RIVER_OPENHT_WORKER_THREADS: ${RIVER_OPENHT_WORKER_THREADS:-auto}
      RIVER_OPENHT_DIRECT_BLOCKS: "${RIVER_OPENHT_DIRECT_BLOCKS:-auto}"
      RIVER_OPENHT_PIPELINE: "${RIVER_OPENHT_PIPELINE:-auto}"
      RIVER_OPENHT_PIPELINE_BATCH_TILES: ${RIVER_OPENHT_PIPELINE_BATCH_TILES:-256}
      RIVER_OPENHT_WORKER_MEMORY_MIB: ${RIVER_OPENHT_WORKER_MEMORY_MIB:-1536}
      RIVER_OPENHT_TIMEOUT_MS: ${RIVER_OPENHT_TIMEOUT_MS:-30000}
      RIVER_OPENHT_MIN_PACKETS: "${RIVER_OPENHT_MIN_PACKETS:-}"
      RIVER_OPENHT_ALLOCATOR_LIBRARY: ${RIVER_OPENHT_ALLOCATOR_LIBRARY-/usr/lib/x86_64-linux-gnu/libmimalloc.so.2}
      RIVER_WEBP_METHOD: ${RIVER_WEBP_METHOD:-0}
      RIVER_CORS_ORIGINS: ${RIVER_CORS_ORIGINS:-http://127.0.0.1:18765,http://127.0.0.1:19074,http://localhost:19074}
      RIVER_RASTER_DISK_CACHE_BYTES: ${RIVER_RASTER_DISK_CACHE_BYTES:-268435456}
      RIVER_S3_RANGE_CACHE_BYTES:
      RIVER_S3_PREPARED_PREFIX_CACHE_BYTES:
      RIVER_PROGRESSIVE_SMALL_PREVIEW_GAIN:
      RIVER_S3_UNINDEXED_COARSE_PREFIX:
      RIVER_S3_REFRESH_PAYLOAD_COSTS:
      RIVER_S3_INDEX_SHARDS: ${RIVER_S3_INDEX_SHARDS:-}
      RIVER_S3_INDEX_LANE_DEPTH: ${RIVER_S3_INDEX_LANE_DEPTH:-}
      RIVER_S3_INDEX_PARTIAL_LANES: "${RIVER_S3_INDEX_PARTIAL_LANES:-auto}"
      RIVER_S3_INDEX_PRIMARY_CHUNKS: "${RIVER_S3_INDEX_PRIMARY_CHUNKS:-auto}"
      RIVER_S3_INDEX_BATCH_SELECTED: "${RIVER_S3_INDEX_BATCH_SELECTED:-auto}"
      RIVER_S3_RANGE_TASKS: ${RIVER_S3_RANGE_TASKS:-auto}
      RIVER_S3_RANGE_CONCURRENCY: ${RIVER_S3_RANGE_CONCURRENCY:-}
      RIVER_S3_INDEX_PROBE_BYTES: ${RIVER_S3_INDEX_PROBE_BYTES:-16777216}
      RIVER_CORPUS_ROOT: /opt/river/corpus
      RIVER_MOSAIC_ROOT: /data
      RIVER_ASSET_CATALOG: ${RIVER_ASSET_CATALOG:-}
      RIVER_OTEL_ENABLED: "false"
    ports:
      - "${RIVER_BIND_ADDRESS:-127.0.0.1}:${RIVER_PORT:-18080}:8080"
    volumes:
      - ${RIVER_DATA_DIR:-./data}:/data:ro
      - river-cache:/var/lib/river/s3-cache
      - river-config:/var/lib/river/config
volumes:
  river-cache:
  river-config:
''', encoding='utf-8', newline='\n')
(package / 'compose.gpu.yaml').write_text('services:\n  river:\n    gpus: all\n', encoding='utf-8', newline='\n')
(package / '.env.example').write_text('''COMPOSE_PROJECT_NAME=river-portable
RIVER_BIND_ADDRESS=127.0.0.1
RIVER_PORT=18080
RIVER_DATA_DIR=./data
RIVER_PROFILE=auto
# Optional native Grok process-wide pool. Omit to retain profile defaults.
# RIVER_GROK_NATIVE_THREADS=16
RIVER_S3_SERIAL_TRANSPORT=sdk
RIVER_S3_SOURCE_MAX_REQUESTS=16
RIVER_S3_MAX_RETRIES=2
RIVER_S3_PARSED_HEADER_CACHE_BYTES=16777216
RIVER_TRACE_SOURCE_READS=0
# Optional isolated diagnostics; unset these for normal serving.
# RIVER_PROFILE_SOURCE=1
# RIVER_PROFILE_CODEC=1
# RUST_LOG=river_server=info,jpeg2k=info
RIVER_OPENJPEG_THREADS_PER_DECODE=auto
RIVER_S3_ADAPTIVE_READS=observe
RIVER_S3_PRECINCT_ROI=''' + structural_default + '''
RIVER_S3_MULTIPART_PLT=''' + structural_default + '''
RIVER_S3_MULTIPART_PREFIX=''' + structural_default + '''
RIVER_S3_SINGLE_PREFIX=''' + single_prefix_default + '''
RIVER_S3_INDEX_ONLY_HEADER_WINDOW_BYTES=0
RIVER_S3_INDEX_ONLY_PREFIX=''' + index_only_default + '''
RIVER_S3_STREAMED_PLT_ROI=''' + streamed_roi_default + '''
RIVER_OPENHT_COARSE=''' + coarse_worker_default + '''
RIVER_S3_EXACT_SUPPORT=''' + exact_support_default + '''
RIVER_S3_PRECINCT_REBASE=auto
RIVER_ENABLE_NVIDIA=auto
RIVER_NVJPEG2000_NATIVE_WORKERS=
RIVER_GROK_NATIVE=true
RIVER_GROK_PACKED_VIEWS=true
RIVER_OPENHT_WORKER_THREADS=auto
RIVER_OPENHT_DIRECT_BLOCKS=auto
RIVER_OPENHT_PIPELINE=auto
RIVER_OPENHT_PIPELINE_BATCH_TILES=256
RIVER_OPENHT_MIN_PACKETS=
RIVER_WEBP_METHOD=0
RIVER_CORS_ORIGINS=http://127.0.0.1:18765,http://127.0.0.1:19074,http://localhost:19074
RIVER_RASTER_DISK_CACHE_BYTES=268435456
# Optional shared compressed-byte budget; zero disables shared retention.
# RIVER_S3_RANGE_CACHE_BYTES=0
# Optional complete-quality compact-prefix cache; default16MiB, zero disables.
# RIVER_S3_PREPARED_PREFIX_CACHE_BYTES=0
# Restore intermediate quality gain for remote whole-image single-tile previews.
# RIVER_PROGRESSIVE_SMALL_PREVIEW_GAIN=true
# Experimental unindexed coarsest preview; default off. Validates packet coverage.
# RIVER_S3_UNINDEXED_COARSE_PREFIX=true
# Optional variable-bandwidth protection; may add latency on steady fast sources.
# RIVER_S3_REFRESH_PAYLOAD_COSTS=true
RIVER_S3_INDEX_SHARDS=
RIVER_S3_INDEX_LANE_DEPTH=
RIVER_S3_INDEX_PARTIAL_LANES=auto
RIVER_S3_INDEX_PRIMARY_CHUNKS=auto
RIVER_S3_INDEX_BATCH_SELECTED=auto
RIVER_S3_INDEX_PROBE_BYTES=16777216
''', encoding='utf-8', newline='\n')
(package / 'start.ps1').write_text('''param([switch]$Gpu)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
if (-not (Test-Path -LiteralPath '.env')) {
  Copy-Item -LiteralPath '.env.example' -Destination '.env'
}
New-Item -ItemType Directory -Path data -Force | Out-Null
docker load --input river-image.tar.gz
if ($LASTEXITCODE -ne 0) { throw 'Image load failed' }
$composeArgs = @('compose','-f','compose.yaml')
if ($Gpu) { $composeArgs += @('-f','compose.gpu.yaml') }
& docker @composeArgs up -d --wait
if ($LASTEXITCODE -ne 0) { throw 'River did not become healthy' }
Write-Output 'River UI and API: http://localhost:18080 (unless overridden in .env)'
''', encoding='utf-8', newline='\n')
(package / 'start.sh').write_text('''#!/bin/sh
set -eu
cd "$(dirname "$0")"
if [ ! -f .env ]; then
  cp .env.example .env
fi
mkdir -p data
docker load --input river-image.tar.gz
if [ "${1:-}" = "--gpu" ]; then
  docker compose -f compose.yaml -f compose.gpu.yaml up -d --wait
else
  docker compose -f compose.yaml up -d --wait
fi
echo 'River UI and API: http://localhost:18080 (unless overridden in .env)'
''', encoding='utf-8', newline='\n')
(package / 'DEPLOY.txt').write_text('''River combined UI + API, Linux/amd64. Docker Engine/Desktop and Compose v2 required.
Windows: powershell -NoProfile -File ./start.ps1
Linux: sh ./start.sh
Optional NVIDIA: ./start.ps1 -Gpu or sh ./start.sh --gpu, with NVIDIA Container Toolkit and a compatible host driver.
The start script copies .env.example when .env is missing. UI and API share http://localhost:18080.
Compatibility tiles default to black-padded 512x512 output when geometry overrides are omitted; edge=crop returns natural edge dimensions and edge=transparent pads with alpha. Regular WMTS defaults to cropped edges and accepts EDGE=crop, EDGE=black or EDGE=transparent. WebSocket clients use /ws?edge=black or /ws/tiles?edge=transparent; regular WS defaults to crop. Full interior tiles share encoded cache entries across modes; genuinely padded edges remain separate.
Crystal geometry: supply tileSize and maxZoom together on /streamer/v1/{z}/{x}/{y}/tile.{ext}; minZoom defaults to 0 and tilePixelRatio to 1. sourceSpan=tileSize*2^(maxZoom-z); bitmapEdge=round(tileSize*tilePixelRatio). Example: /streamer/v1/7/2/1/tile.webp?image=s3%3A%2F%2Fbucket%2Fimage.jp2&tileSize=256&maxZoom=7&tilePixelRatio=2&edge=transparent returns a 512px bitmap covering a 256px source footprint. Encode the whole image URL once as an outer query value. Native resolution counts do not change this client pyramid. Padding follows resampling; crop clips to image edges. WMTS and WebSocket geometry is unchanged.
For LAN access, set RIVER_BIND_ADDRESS=0.0.0.0 in .env and run the script again.
For internet access, place an authenticated TLS reverse proxy in front of River. River development APIs have no built-in authentication.
Place your unchanged images in ./data or set RIVER_DATA_DIR to an absolute host path. The data mount is read-only. Images and S3 credentials are not bundled.
New bundles default to the stable Compose project name river-portable, so changing the extracted folder name does not create different configuration/cache volumes. Set COMPOSE_PROJECT_NAME in .env for separate instances (and use separate ports).
Upgrading: copy the previous .env and preserve COMPOSE_PROJECT_NAME. Bundles before this change used a folder-derived project name: find the existing name with docker compose ls and set COMPOSE_PROJECT_NAME to that exact name in the new .env before starting. Keep RIVER_DATA_DIR pointing to the same images directory; an absolute path avoids folder-relative changes. Stop the old stack normally before starting its replacement.
The named river-cache volume retains technical caches across container replacement. Do not use compose down -v if you want to retain it.
S3 sources can be connected in the UI. Definitions and credentials persist privately in the river-config volume; protect this volume as configuration containing secrets. Credentials are not returned by listing APIs. Do not delete this volume if you want to retain connections.
auto is the default profile: CPU quota and memory-aware admission, native Grok with CLI compatibility fallback, faster WebP encoding. RIVER_PROFILE=compact retains conservative concurrency. RIVER_WEBP_METHOD in .env is the startup fallback when no saved encoding setting exists; RIVER_GROK_NATIVE=false selects CLI-only decoding.
The openhtj2k driver is selectable in the viewer, WMTS DRIVER, compatibility driver query, and WebSocket driver field. It uses an isolated reusable OpenHT worker for conservatively supported coarsest-resolution packet views, with Grok for other layouts and small jobs. Existing HT-coded streams use the Grok fallback; this accelerator also decodes ordinary JPEG 2000 without changing its entropy data. Response routes identify openht-packed-ranges or openht-fallback-s3-packet-ranges/openht-fallback-s3-tile-part-ranges/openht-fallback-s3-disk-compatibility. Worker errors are explicit; source images remain unchanged. One worker is the default. Automatic native threads use half the available CPU budget divided across workers, capped at sixteen when direct blocks are enabled (eight otherwise). Automatic direct frame budgets fall back to at most eight while another source preparation or worker request is pending. Compact retains two; an explicit numeric value overrides adaptation. increase RIVER_OPENHT_WORKERS only after measuring capacity and memory.

WebP method is configurable live: GET /api/settings/encoding; PUT /api/settings/encoding with JSON {"webpMethod":4}. Methods 0 through 6 are accepted. The viewer provides Server encoding controls without a token. The saved setting overrides the startup environment fallback and persists in river-config/configuration.json. Cache entries are separated by method; in-flight render groups retain their snapshot.
The viewer opts into early spatial preview patches for large remote images. Legacy WebSocket clients retain the original tile grid unless they send progressivePatches:true. Final tiles retain the original grid. Cached previews use compact tiles to keep repeated loads fast.
The optional RIVER_S3_RANGE_CACHE_BYTES setting in .env overrides the shared compressed-byte budget; zero disables that cache. If omitted, the runtime profile chooses its normal budget. A reader-local buffer retains at most 16 KiB of already-read blocks during a preparation; it is discarded afterward. Header, index and rendered-tile caches have independent budgets.
Encoded tiles persist in a bounded 256 MiB cache. RIVER_RASTER_DISK_CACHE_BYTES=0 disables it. Version changes invalidate entries. Original imagery remains unchanged.
Large unindexed cold loads can use bounded parallel index walks after a bandwidth pilot approves. Large grids may probe on nearby storage; ordinary grids retain a 2 ms latency threshold. Concurrent nearby cold sources can suppress speculative probes. RIVER_S3_INDEX_SHARDS=0 disables the bandwidth-for-latency tradeoff; automatic 4-32 chains for qualifying large whole-image walks (4 otherwise), max32. RIVER_S3_INDEX_LANE_DEPTH is automatic when empty: qualifying nearby large-grid whole-image requests advance bounded chunks within the same runtime task; use1 to retain one-step waves, or an explicit1-128 for diagnostics. Concurrent-source, byte, request and deadline guards remain active. RIVER_S3_INDEX_PROBE_BYTES caps extra probe traffic (default/max 16777216 bytes, also within a quarter of the remaining read budget). A timed 64 KiB pilot skips bulk probing when its estimated bandwidth cost exceeds likely latency savings; a slow pilot falls back after 500 ms. These .env controls are passed to the container. Original image bytes are unchanged; speculative offsets must connect to the real contiguous source chain before use.
Stop: docker compose down. Logs: docker compose logs -f river.
This bundle includes CPU OpenJPEG/JP2Grok/OpenHTJ2K hybrid, optional NVIDIA support, the viewer, OpenAPI UI and third-party license notices inside the image.
''', encoding='utf-8', newline='\n')
manifest = dict(status, archive='river-image.tar.gz', platform='linux/amd64', includes=['backend', 'web UI', 'CPU codecs', 'optional NVIDIA codecs'])
if configuration_profile is not None:
    (package / 'CONFIGURATION-PROFILE.json').write_text(json.dumps(configuration_profile, indent=2), encoding='utf-8', newline='\n')
    manifest['configurationProfile'] = 'CONFIGURATION-PROFILE.json'
    example = package / '.env.example'
    text = example.read_text(encoding='utf-8')
    assert 'RIVER_S3_SERIAL_TRANSPORT=sdk' in text
    example.write_text(text.replace('RIVER_S3_SERIAL_TRANSPORT=sdk', 'RIVER_S3_SERIAL_TRANSPORT=auto'), encoding='utf-8', newline='\n')
    with (package / 'README.md').open('a', encoding='utf-8') as readme:
        readme.write('\nThis configuration-profile bundle defaults to auto serial transport for fresh installations. Existing .env and saved API settings retain their values. Read SERIAL-AUTO-PROFILE.md and review apply-serial-transport.py before changing an existing installation. The backend image is unchanged.\n')
release = json.loads((root / 'deploy/releases.json').read_text(encoding='utf-8'))
manifest['releaseChannels'] = []
for channel, identity in [('latest', release.get('latestImageId')), ('stable', release.get('imageId')), ('lts', release.get('ltsImageId'))]:
    if identity == status['imageId']:
        manifest['releaseChannels'].append(channel)
(package / 'release.json').write_text(json.dumps(release, indent=2), encoding='utf-8', newline='\n')
shutil.copyfile(root / 'deploy/benchmark-policy.md', package / 'BENCHMARK-POLICY.md')
(package / 'BUILD-MANIFEST.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8', newline='\n')
with (package / 'README.md').open('a',encoding='utf-8',newline='\n') as readme:
    readme.write("\nThe openhtj2k driver can directly decode eligible classic Part1 packed LL blocks with bounded scratch memory instead of constructing a full precinct hierarchy. Originals are unchanged. Valid unsupported packed layouts retain the existing decoder. Direct decoding writes normalized samples into RGBA from component-aligned precinct groups without full sample planes; larger eligible blocks also use this path. RIVER_OPENHT_DIRECT_BLOCKS=true forces a direct attempt; false disables it. With the automatic packet threshold, compatible rasters of at least 65536 pixels can use the worker even below 16384 packets; tiny rasters retain the prior route. An explicit RIVER_OPENHT_MIN_PACKETS value overrides automatic selection (0 forces worker selection for diagnostics). These controls are passed through from .env.\n")

with (package / 'README.md').open('a',encoding='utf-8',newline='\n') as readme:
    readme.write("\nQualifying large unindexed partial views can use bounded deeper index lanes as well as whole-image previews. Walks pause at requested missing tile parts and commit only offsets connected to the real Psot chain. RIVER_S3_INDEX_PARTIAL_LANES=false restores the prior partial-view scheduling; RIVER_S3_INDEX_LANE_DEPTH=1 disables deeper chunks. Existing version checks, read/request/deadline budgets and the concurrent-source guard remain active. Image contents and the normal source-paging controls are unchanged.\n")

with (package / 'README.md').open('a',encoding='utf-8',newline='\n') as readme:
    readme.write("\nKnown-cursor index chunks can continue a qualifying large unindexed walk even after speculative lanes are exhausted. This follows necessary Psot headers only and adds no bulk probes. It retains the prior probe decision, requested-tile stops, version checks, read budgets, deadlines and concurrent-source guard. RIVER_S3_INDEX_PRIMARY_CHUNKS=false restores prior handling; the default is auto.\n")

performance = args.performance_report
if performance is not None:
    shutil.copyfile(performance, package / 'PERFORMANCE.json')
    performance_record = json.loads(performance.read_text(encoding='utf-8'))
    if performance_record.get('qualifiedForPromotion') is True:
        manifest['qualificationEvidence'] = {}
        for index, (name, evidence) in enumerate(performance_record.get('evidence', {}).items()):
            source = (root / evidence['path']).resolve()
            source.relative_to(root.resolve())
            assert hashlib.sha256(source.read_bytes()).hexdigest() == evidence['sha256'], name
            relative = f'qualification-evidence/{index}{source.suffix or ".json"}'
            destination = package / relative
            destination.parent.mkdir(exist_ok=True)
            shutil.copyfile(source, destination)
            manifest['qualificationEvidence'][name] = dict(path=relative, sha256=evidence['sha256'])
        (package / 'BUILD-MANIFEST.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8', newline='\n')
    if performance_record.get('nativeGrokPoolExperiment'):
        with (package/'README.md').open('a',encoding='utf-8') as readme:
            readme.write('\nNative Grok shared-pool experiment: this bundle forwards optional RIVER_GROK_NATIVE_THREADS from .env. Set a numeric value, then recreate the container; the pool initializes once per process. Omit the setting to retain existing profile defaults. This is separate from CLI RIVER_GROK_THREADS_PER_DECODE. Use driver=jp2grok on compatibility requests to select Grok. PERFORMANCE.json records the exact16logicalCPU/4GiB/single-admitted-decode experiment, cold pixels, reads, bytes and timing. It is a configuration comparison, not a new decoder binary or an OpenJPEG speed claim. Source images, saved S3 connections and settings volumes are unchanged.\n')

with (package / 'README.md').open('a',encoding='utf-8') as readme:
    readme.write("\nCheckpoint224 extends bounded native-tile decode overlap to supported coarsest partial selections, including sparse and mixed-cache WS batches. Automatic nearby index chunks use depth128 for partials and32 for whole views. RIVER_S3_INDEX_BATCH_SELECTED=false restores earlier primary stops. RIVER_OPENHT_PIPELINE_BATCH_TILES defaults to256 (32-512), with two queued batches. TLM-indexed coarsest requests fuse bounded selected header/prefix reads. RIVER_OPENHT_PIPELINE=auto is the default; false disables it. Dense tiny-block layouts, clipped native grids and explicit worker-selection overrides retain existing routes. RIVER_OPENHT_DIRECT_BLOCKS=false also disables the pipeline. The route is openht-pipeline-ranges. decodeMs includes native work performed during preparation; sourceCacheMs is preparation wall time including overlap, so these durations must not be added. Qualifying normal WebSocket groups can use the pipeline; concurrent work may retain fallback. Indexed individual-tile speedups do not imply equivalent full-viewport or arbitrary organizational-S3 latency. Source images remain unchanged.\n")

if args.enable_structural_reads:
    with (package / 'README.md').open('a', encoding='utf-8') as readme:
        readme.write('\nStructural-read candidate: this bundle enables RIVER_S3_PRECINCT_ROI, RIVER_S3_MULTIPART_PLT and RIVER_S3_MULTIPART_PREFIX. Each can be set to false independently. Precinct ROI requires compatible zero-origin reversible Part1 files. Checkpoint680 also supports eligible multiple quality layers; coordinate rebasing remains single-layer. Multipart PLT continues packet addresses across validated tile-part chains. The early prefix path requires explicit TLM tile IDs, one quality layer and compatible resolution-prefix progression; it skips later parts only after the requested prefix is complete. Partial precinct selections retain spatial packet reads. Unsupported layouts keep the compatibility paths. Automatic aligned technical windows are enabled for eligible huge single-native-tile sources; set RIVER_S3_PRECINCT_REBASE=false to retain the original decoder geometry. Release channels are recorded in the repository release manifest after qualification. The accompanying performance report states source layout and benchmark limitations.\n')

if args.enable_single_prefix:
    with (package / 'README.md').open('a', encoding='utf-8') as readme:
        readme.write('\nRIVER_S3_SINGLE_PREFIX=true enables bounded PLT traversal and entropy-preserving coarse projections for compatible zero-origin, single-native-tile, single-layer Part1 sources at their coarsest resolution. It supports reversible and irreversible wavelets; unsupported layouts retain existing paths. Original imagery is unchanged. This does not promise low GET counts for fragmented multi-native-tile previews or deep tiles. Set false to disable. Release channels follow the repository release manifest.\n')

if args.enable_coarse_worker:
    with (package / 'README.md').open('a', encoding='utf-8') as readme:
        readme.write('\nRIVER_OPENHT_COARSE=true lets the openhtj2k driver decode eligible zero-DWT reversible coarse streams with multiple codeblocks per precinct and reversible RGB transforms. Supported streams remain bounded to 4 million pixels, 66 MiB compressed input and unsigned components up to 16 bits. Existing packed/direct decoding stays preferred. Small requested regions keep the region decoder; an explicit RIVER_OPENHT_MIN_PACKETS value overrides automatic worker selection. Unsupported layouts retain existing paths. Set RIVER_OPENHT_COARSE=false to disable this candidate path. Original entropy and source images are unchanged.\n')

if args.enable_exact_support:
    with (package / 'README.md').open('a', encoding='utf-8') as readme:
        readme.write('\nRIVER_S3_EXACT_SUPPORT=true enables per-band inverse 5/3 support selection for eligible zero-origin, single-native-tile, single-layer reversible Part1 sources. It avoids treating a large conservative margin as whole-image demand, which could otherwise read a complete native tile for a small request. Plans must fit the metadata budget and predict material precinct savings; unsupported layouts or unhelpful plans retain the existing mask. Set false to disable. Source files and retained packet entropy are unchanged. This does not guarantee 2-5 GETs for fragmented packet layouts, nor solve files lacking usable packet indexes. Release channels follow the repository release manifest.\n')

with (package / 'README.md').open('a',encoding='utf-8') as readme:
    readme.write('\nRIVER_S3_PRECINCT_REBASE=auto reduces decoder geometry for eligible one-layer, zero-origin, reversible Part1 sources with a usable single-part PLT index. Precinct alignment and the full-depth wavelet support envelope preserve requested pixels; irrelevant packets inside the temporary window remain empty. This covers reduced preview passes as well as final tiles. Automatic windows require at least64 million original pixels, at most256 million window pixels and at least4x area reduction. SOP/EPH and unsupported layouts retain ordinary decoding. Source files remain unchanged. Set false to disable; true explicitly selects eligible windows without the automatic size gate.\n')

install_handoff(package)
codec_profile_source = build / 'source/crates/river-openjpeg-sys/vendor/src/lib/openjp2/tcd.c'
if codec_profile_source.exists() and 'RIVER_PROFILE_CODEC' in codec_profile_source.read_text(encoding='utf-8'):
    with (package / 'README.md').open('a', encoding='utf-8', newline='\n') as readme:
        readme.write('\nOptional isolated codec diagnosis: add RIVER_PROFILE_SOURCE=1, RIVER_PROFILE_CODEC=1 and RUST_LOG=river_server=info,jpeg2k=info to .env, then recreate River. These values are forwarded by Compose. Codec stages use monotonic wall time, including worker waits; they are not CPU time. Normal serving leaves these variables unset. Diagnostic logging adds overhead and can emit many short lines for tiled images, so keep it separate from release timing runs. Preserve existing configuration/S3 credentials and volumes. This experimental bundle does not change stable/LTS release pins.\n')
worker_policy_source = build / 'source/crates/river-server/src/decoder/thread_budget.rs'
if worker_policy_source.exists() and 'let runnable=active.max(1).min(slots.max(1))' in worker_policy_source.read_text(encoding='utf-8'):
    with (package / 'README.md').open('a', encoding='utf-8', newline='\n') as readme:
        readme.write('\nLarge OpenJPEG jobs share workers across active requests that can occupy decode slots. Queued requests do not reserve workers for slots they cannot use. Moderate jobs retain a maximum of four, and the global worker lease bound still applies. Keep RIVER_OPENJPEG_THREADS_PER_DECODE=auto and RIVER_DECODE_CONCURRENCY=auto for CPU-based sizing; an explicit worker maximum of1or4 remains available. Full-image compact views with many independent native-tile groups can also use spare workers even when the output is small. Read PERFORMANCE.json for the exact build timings,qualification and limitations; results from concurrent or supplemental workloads are not universal single-request speedup claims. Retain existing S3/configuration volumes.\n')
if args.enable_streamed_plt_roi:
    with (package / 'README.md').open('a', encoding='utf-8', newline='\n') as readme:
        readme.write('\nRIVER_S3_STREAMED_PLT_ROI=true streams large single-native-tile PLT tables into bounded spatial demand, preserving an index in the temporary codestream. It requires compatible zero-origin, one-layer reversible RPCL files, precinct ROI, and one to eight million packets. It preserves original source bytes. Set false in .env to disable. This does not guarantee a fixed GET count or support every JPEG2000 layout.\n')
if args.enable_index_only_prefix:
    with (package / 'README.md').open('a', encoding='utf-8') as readme:
        readme.write('\nRIVER_S3_INDEX_ONLY_PREFIX=true supports compatible reduced previews with PLT pointers in empty tile-parts before the packet data. It retains only the requested prefix and validates the intervening marker chain. Source files remain unchanged. This is a candidate feature; metadata GET counts remain layout dependent. Set false in .env to disable.\n')
policy_source=build/'source/crates/river-server/src/read_policy.rs'
if policy_source.exists() and 'pub enum IndexHeaderMode' in policy_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nIndex-only header policy API: GET /api/settings/read-policy and retain its settings object. To opt in, set settings.mode=adaptive and settings.indexHeaders={"mode":"adaptive","windowBytes":1048576,"maxExtraBytes":33554432}, then PUT that complete settings object to /api/settings/read-policy. This persists across restarts and complete configuration export/import. Preserve existing S3 connections and other settings; do not replace the configuration with a sample. Unknown connections stay sparse until enough real measurements exist. GET /api/settings/read-policy exposes sources.<id>.indexHeaderDecision with the reason, sample counts and estimated costs. Header modes are sparse, dense and adaptive; API settings override the legacy environment window. Dense can regress badly on slow links. The explicit metadata byte allowance conservatively counts whole windows, separately from packet overfetch; existing source limits remain active. Omit indexHeaders to retain legacy behavior. Measurements are process-local. This candidate has no automatic stable/LTS promotion.\n')
if (build/'source/crates/river-server/src/remote_source/streamed_header.rs').exists():
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nThis candidate also supports streamed windows: select indexHeaders.windowBytes=8388608 (8MiB), mode=adaptive and maxExtraBytes=33554432 (32MiB) in the existing read-policy settings object. Windows above1MiB consume larger physical ranges but retain at most256KiB of marker and requested packet-prefix blocks per window. Every marker is still validated. Unknown connections remain sparse; real previous requests train the process-local source profile. A fast cached source showed a gain; a slow link can favor sparse reads. These candidate measurements do not promote stable or LTS. Preserve all existing S3 connections, credentials, settings and volumes when deploying.\n')
prefix_source=build/'source/crates/river-server/src/remote_source/multipart_prefix.rs'
if prefix_source.exists() and 'Retain all quality packets of the selected resolutions' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nQuality-limited progressive previews: the TLM resolution-prefix path now handles first/intermediate quality-layer requests on supported RLCP/RPCL sources by retaining all quality packets of the selected low resolutions. Original layer counts and packet bytes are preserved; the decoder enforces the requested layer limit. PLT and supported inline headers are handled. Partial-quality sources at or below the existing8MiB small-object threshold keep the established small-object path to avoid adding many GETs. Large fragmented sources can still require many part requests; consult the included performance report for physical GET counts and regressions. This candidate is not a stable/LTS promotion.\n')
if prefix_source.exists() and 'fn small_prefix_probe' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nBounded tile-part read-ahead: eligible large multilayer TLM previews can propose up to three adjacent small parts together, at most16KiB per probe. Every part and packet boundary is still parsed; the proposal never substitutes for validation. A plan that exceeds remaining byte or request-local lease capacity falls back to the previous probes. Noncontiguous and large parts retain their normal handling. This reduces request waves without modifying source images. It does not promise2-5 GETs or a timing gain on every source.\n')
prefix_source=build/'source/crates/river-server/src/remote_source/multipart_prefix.rs'
if prefix_source.exists() and 'clustered_prefix_probe' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nMultilayer prefix reads: when validated TLM offsets prove that initial small tile-parts form a contiguous run, River can prefetch that run in one bounded wave. Every SOT, coding header and packet count is still validated. Small parts already pinned in memory are parsed from one buffer, reducing repeated cached probes without additional source reads. Cancellation is checked during packet traversal. The included report distinguishes native-major and resolution-major layouts, unchanged bytes, exact quality layers and source latency profiles. This does not guarantee two GETs for scattered or unindexed inputs, and production source files are not reordered. Set RIVER_PROFILE_SOURCE=1 for opt-in packet-order, probe-read, packet-parse and stream-build timings.\n')
engine_source=build/'source/crates/river-server/src/engine.rs'
remote_source=build/'source/crates/river-server/src/remote_source.rs'
if remote_source.exists() and 'fn lowest_prefix_window' in remote_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nUnindexed coarse previews: contiguous lowest-resolution packet prefixes use bounded growing read-ahead after consuming useful bytes, instead of one dependent GET per packet. Every packet header is still parsed; windows shrink to the remaining preparation byte budget. Small packet counts keep the existing tiny probes. The included report records the GET/byte tradeoff on an entropy-preserving diagnostic copy of an11GB source. This is not a promise for all unindexed layouts, arbitrary higher-resolution requests, or organizational-S3 latency. Production source files and default budgets are unchanged. Preserve existing S3 connections, persisted settings, and configuration/cache volumes.\n')
if remote_source.exists() and 'prefix_read_ahead(wanted).max(lowest_prefix_window' in remote_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nIntermediate unindexed resolution prefixes also use budget-aware growing read-ahead. Cancellation is checked for every packet. This reduces fragmented source reads for eligible single-layer resolution prefixes; source files are unchanged. Full-resolution sparse requests on unindexed monolithic native tiles remain limited, and this candidate does not remove the disk-compatibility limit. Consult the included report for exact GET counts, bytes, slower cases, and the separately scoped Kakadu demo adapter observations.\n')
if remote_source.exists() and 'inline spatial filtering requires one quality layer' in remote_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nUnindexed spatial packet selection: eligible one-layer reversible Part1 partial views now pass the existing precinct support mask into the inline prefix reader. Required packet headers are parsed, selected entropy is preserved, and unrelated packets become legal empty packets in the temporary decoder input. Production source objects are unchanged. This can reduce preparation/decode work without a corresponding reduction in wire bytes; the included report distinguishes both. RIVER_PROFILE_SOURCE=1 logs parsed/kept packet counts, omitted stream bytes, and decoder-input size. Omitted stream bytes are not a measure of source bandwidth saved. Existing RIVER_S3_PRECINCT_ROI=false disables spatial filtering.\n')
if prefix_source.exists() and 's3-multipart-compact-lowest-prefix' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        levels='lowest three resolutions' if 'wanted<=2' in prefix_source.read_text(encoding='utf-8') else 'coarsest resolution'
        readme.write('\nCompact multilayer previews: eligible full-grid reversible Part1 prefixes preserve original packet bytes and quality layers while projecting temporary SIZ/COD/quantization headers to the '+levels+'. This avoids synthetic high-resolution empty packets and their decoder geometry. Source objects and default budgets are unchanged. The included14.21GB periodic-content TLM test is a diagnostic layout,not the organizational corpus;see the report for exact256/512/1024pixel requests and configuration differences. Eligible sparse-grid behavior is described below. OpenJPEG remains the default;CPU results do not validate GPU support. Previously observed GPU preparation failures remain an open limitation. Stable/LTS channels are unchanged.\n')
reduced_source=build/'source/crates/river-server/src/decoder/reduced_tiles.rs'
if reduced_source.exists() and 'Vec<Vec<Part>>' in reduced_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nMultipart native parallel decoding validates each tile-part chain before decoding independent native tiles using the existing shared CPU budget. Each native codec remains single-threaded. No global worker/concurrency default is changed. The report separates code improvements at four CPUs from an eight-CPU/eight-worker configuration test. That single-image latency configuration is not a general throughput recommendation. Preserve existing S3 settings,credentials,and configuration/cache volumes.\n')
if prefix_source.exists() and 's3-multipart-partial-compact-prefix' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nSparse reduced views: eligible partial native grids now use projected temporary headers without changing original tile indices,source bytes,or quality layers. Single rectangular partial requests can use the existing CPU-budgeted native parallel renderer;it validates every required tile and clips decoded pieces to the requested rectangle. Other sparse request shapes keep the ordinary ROI decoder. This removes decoder padding work. Consult the included performance report for the current GET count; source planning changes are measured separately. Do not interpret low local latency as proof for a distant or loaded S3.\n')
if engine_source.exists() and 'fn bounded_gpu_cpu_view' in engine_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nGPU previews: start with ./start.ps1 -Gpu or sh ./start.sh --gpu, then explicitly request driver=nvjpeg2000. OpenJPEG remains the default. Eligible single-native-tile, one-layer previews reuse original low-resolution packet bytes in a compact temporary codestream; source objects are unchanged. Validated classical ROI views from oversized native grids use OpenJPEG with x-river-decoder-route=openjpeg-gpu-large-grid-fallback. Other inputs retain existing driver handling. GPU results are hardware-specific; see the performance report for all three sizes, slower cases, exact pixels, GETs/bytes and measured cache state. Preserve existing S3 settings and config/cache volumes.\n')
if 'prefix_bootstrap_count' in (build/'source/crates/river-server/src/remote_source.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nUseful prefix bootstrap: adaptive mode can fetch the required ranges needed to reach the configured sample threshold before replanning the remainder. No calibration probes or raised budgets. Source latency and bandwidth can lead to different read plans; consult the included report for exact GETs, bytes, slower cases, and comparison scope. Organizational422/timeouts remain unresolved. Preserve existing source settings and volumes.\n')
if engine_source.exists() and 'saturating_mul(u64::from(l.columns))' in engine_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nLarge GPU grids: the guard accounts for all native tiles, allowing classical selective prefix preparation on huge multi-tile images. Validated large compact grids and sparse compact views use the existing bounded CPU decoder and explicitly report openjpeg-gpu-large-grid-fallback. HTJ2K keeps its required complete-grid handling. This is an experimental candidate, not a stable/LTS promotion. Consult the included report for the exact comparison scope, failures, successful timings, and slower cases. It does not establish an organizational OpenJPEG fix.\n')
if prefix_source.exists() and 'h.cod[8]&0xc0==0' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nClassical codeblock styles: eligible reversible multilayer compact prefixes now preserve styles0through63, including TERMALL/style4, rather than requiring style0. Every retained style bit, quality layer, and entropy byte stays intact; HT/reserved styles and the other eligibility checks are unchanged. All64styles were tested against original-file pixel references. The structural tests explicitly disable the small-object bypass; the normal8MiB threshold remains unchanged in this bundle. The new14.336GBperiodic style4 fixture is synthetic and separately versioned, not an organizational file. Its measured GETs/bytes are unchanged; the gain removes temporary high-resolution packet padding and decoder work. Organizational422/timeouts and large unindexed full-resolution limitations remain open. Preserve existing S3 settings and config/cache volumes.\n')
if remote_source.exists() and 'fn tlm_header_read_end' in remote_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nMain-header diagnostics now label mainHeader reads separately from initialHeader and payload reads. In adaptive mode, large TLM segments can warm a bounded half-segment tail; observe/disabled modes, a1xoverfetch cap, or insufficient header budget retain exact reads. This is structural lookahead, not a lowered learning threshold. The included327versus328experiment has fewer GETs for complete previews with unchanged bytes and mixed latency results; it is not a blanket speedup.\n')
if remote_source.exists() and 'saturating_add(materialized)<=MAX_PACKETS' in remote_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nFine-precinct allocation fix: the packet-order memory guard now counts the prefix/layers actually materialized, rather than rejecting a coarse view because unrelated resolutions contain more than1million packets. Full materialized indexes retain their existing cap. No budget is raised. A new19MB16bit/16layer/TERMALL/TLM fixture encoded with Kakadu reproduced four422responses on the previous backend; this candidate serves them with exact independently decoded pixels and1to2GETs. It also passes with the normal8MiBsmall-file threshold and with the shared range cache disabled. This is a format/correctness regression, not an organizational latency comparison; the unchanged three-size benchmark and slower cases are included. No Kakadu demo binary/library or generated diagnostic imagery is bundled. Preserve existing S3 settings and configuration/cache volumes.\n')
if prefix_source.exists() and 'let eligible=h.layers>1 && !h.is_ht' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nAdditional retained resolutions: validated reversible multilayer compact projection now extends beyond the three lowest resolutions. Every original selected coefficient packet and quality layer is preserved. Other format/alignment/header checks and existing resource budgets remain unchanged. A separately scoped2048square request on the existing14.336GBstyle4 diagnostic measured329versus330; consult the included report for exact paired timings, GET variation, bytes, and all three frozen-size regressions. This removes decoder padding work, not source data. It is not a stable/LTS promotion or organizational latency claim.\n')
policy_source=build/'source/crates/river-server/src/read_policy.rs'
if policy_source.exists() and 'pub struct ContiguousDecision' in policy_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nExperimental contiguous transport: adaptive mode may join adjacent required blocks into ranges up to4MiB after the configured minimumSamples and three measured body transfers. This adds no gap bytes. A125msbody-time estimate limits growth;the existing1MiBfloor remains for slow/unknown sources. The lower of latest and average body rates controls this cap. Read /api/settings/read-policy sources.*.contiguousRanges for the last decision and sources.*.lastBodyBytesPerSecond for the latest measured rate. The learning object exposes the size/estimate limits and exact timeout scopes. Observe/disabled modes preserve the old cap. This candidate reduced15to10GETs on the scoped14GBrequest but its local latency worsened;it is not a general speed upgrade or stable/LTS promotion.\n')
if remote_source.exists() and 'body_with_progress_timeout' in remote_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nBatched SDK timeouts: response headers and idle body progress each retain10seconds;the entire operation remains bounded by its existing deadline/cancellation. Continuing data may legitimately take more than10seconds. Byte overflow,truncation,and empty-chunk stall checks remain. A private source-slowdown control verified a successful progressing transfer beyond10seconds,exact pixels,and a subsequent1MiBcap backoff. This is not an organizational-source performance claim. Preserve existing S3 settings and configuration/cache volumes.\n')
if remote_source.exists() and 'fn parallel_contiguous_limit' in remote_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nTransfer-lane preservation: learned contiguous chunk growth is additionally limited by required bytes per useful executor/source lane. This retains parallel reads rather than collapsing a large run into too few connections. Only adjacent required blocks join;the byte union,learning thresholds,source concurrency,overall budgets,and progress-aware timeouts remain unchanged. The API last-decision reason can be preserveTransferParallelism. The scoped preview is modestly faster versus331and330,but uses12GETsversus331s10. Bytes are unchanged. Frozen timings are mixed,including a slower11GBdeep result and its focused recheck;this is not a stable/LTS promotion.\n')
reduced_source=build/'source/crates/river-server/src/decoder/reduced_tiles.rs'
if reduced_source.exists() and 'fn grouped_jobs' in reduced_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nGrouped native codec contexts: adjacent requested tiles on one native row can share one OpenJPEG codec,up to4or the lower RIVER_MAX_NATIVE_TILES_PER_JOB. Native tile dimensions,reference coordinates,tile-part chains,entropy bytes,and the shared CPU budget remain unchanged. Groups stop at row boundaries/gaps;clipped edges retain their true dimensions. The scoped2048square preview reduces4096codec contexts to1024. The report includes exact pixels,partial/quality/16bit/clipped checks,and mixed frozen timings. This is an experimental candidate,not stable/LTS or an organizational speed claim.\n')
layered_source=build/'source/crates/river-server/src/remote_source/layered_packets.rs'
if layered_source.exists() and 'pending_grids' in layered_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nLazy packet-band state: empty quality packets do not allocate tag trees/codeblock arrays. The first nonempty packet initializes them inside the existing transactional parse,so failed/short probes leave state untouched. Layer ordering,metadata admission,source bytes,read budgets,and worker limits remain unchanged. The report includes delayed inclusion checks across64styles,independent pixels,partial/16bit/progressive validation,and all three frozen sizes including slower cases. This is an experimental candidate,not stable/LTS or an organizational latency claim.\n')
if prefix_source.exists() and 'fn cached_part_bytes' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nComplete cached packet parts: already cached parts up to16KiBcan be leased from request/shared caches and copied once without source IO. Missing/short blocks retain the ordinary reader. The full immutable buffer supports nontransactional packet parsing only because any error aborts preparation immediately;unbuffered/growing probes retain state rollback. No network data is fetched to enable this path. Source bytes,metadata admission,worker and read budgets remain unchanged. The report separates profiled read/parse work from profiling-off paired latency and retains slower frozen cases. Experimental,not stable/LTS or organizational parity.\n')
if prefix_source.exists() and 'fn prefix_cache_limit' in prefix_source.read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nComplete-quality prepared prefix cache: compatible compact TLM multipart views retain all quality layers and can be reused between gain/final passes with the same source version, reduction and native selection. The shared cache defaults to16MiB, admits entries up to4MiB and has at most128entries. Partial/spatial/legacy views are excluded. Set RIVER_S3_PREPARED_PREFIX_CACHE_BYTES=0 in .env to disable, or set a budget up to64MiB, then recreate the container. This cache is independent of compressed source-byte and rendered-tile caches; it does not rewrite source images. PERFORMANCE.json reports first-frame and final-frame timings separately.\n')
if 'RIVER_PROGRESSIVE_SMALL_PREVIEW_GAIN' in (build/'source/crates/river-server/src/main.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nSmall whole-image progressive previews: remote requests covering the complete image in one output tile send a preview followed by the exact final, avoiding a serial intermediate quality decode. Larger/multiple-tile requests and local pyramid plans retain their gain pass. Set RIVER_PROGRESSIVE_SMALL_PREVIEW_GAIN=true in .env and recreate the container to retain gain for small previews too. Final quality and source images are unchanged. Read PERFORMANCE.json for the explicitly matched read-policy comparison; older progressive runs without an explicit policy reset are provisional.\n')
if (build/'source/crates/river-server/src/remote_source/jpip_index.rs').exists():
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nOptional embedded Part9 tile-part indexes: advertised iptr/fidx/cidx pointers may provide tpix/faix offsets when no usable explicit TLM is available. Retrieval reads a bounded prefix and required cptr/tpix children, skipping unused index payloads. Selected offsets are validated against source SOT headers; unusable optional metadata retains bounded discovery. Supported complete indexes also feed the existing resolution-prefix reader for progressive previews. TLM and Part9 indexes remain optional. Tests cover generated indexed fixtures; the frozen50MB/979MB/11GB files have no Part9 indexes. Deep tiles used4-5GETs in those diagnostics; a scattered256-native-tile preview still needed258GETs. This is not a universal GET-count or organizational latency claim. PERFORMANCE.json states measured image versions and outstanding qualification.\n')
if 'fn acquire_render_header' in (build/'source/crates/river-server/src/remote_source.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nDemand-driven optional index loading: metadata-only inspection reads the immutable image header without fetching an advertised Part9 tile index. Rendering loads and validates the index once in the bounded header cache. Cached materialized indexes are preserved for the same immutable header. In the indexed diagnostic, metadata inspection used1GET/3072bytes instead of4GETs/201396bytes; metadata followed by a tile still used5GETs/205492bytes total. This moves necessary index work to rendering and saves it when pixels are never requested; it is not a general cold-image latency improvement.\n')
if 'RIVER_S3_UNINDEXED_COARSE_PREFIX' in (build/'source/crates/river-server/src/remote_source/multipart_prefix.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nExperimental unindexed coarsest previews: RIVER_S3_UNINDEXED_COARSE_PREFIX=true in .env, followed by container recreation, enables a bounded candidate for eligible reversible multilayer whole-image previews. Default off. Every selected SOT and requested packet coverage is validated. The initial first-part candidate is limited to8MiB/128reads inside existing limits. Unsupported ordering/quality retains ordinary discovery and its request limits. PERFORMANCE.json records current outcomes and origin IO; no universal GET-count or organizational latency claim is made. Keep existing S3 connections, credentials and configuration volumes.\n')
if 'fn prepare_discovered_multipart_prefix' in (build/'source/crates/river-server/src/remote_source/multipart_prefix.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nDiscovered multipart prefix reuse: with the experimental flag enabled, an eligible whole-image coarsest preview can feed a completely discovered, contiguous tile-part chain into the compact packet-prefix planner. This does not pretend the file contained TLM. In the generated14MBtile-major diagnostic, this resolved a422failure with exact pixels at257GETs/4459520bytes;257GETs remains excessive. The contiguous-first-part layout retained2GETs/252928bytes. These are supplemental diagnostics, not performance results for organizational imagery.\n')
if 'let depth=usize::from(h.levels-reduction)' in (build/'source/crates/river-server/src/remote_source/multipart_prefix.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nNext-level unindexed previews: the same opt-in flag also permits eligible reduced previews up to512x512pixels to collect a bounded run of early tile-parts. Ordinals limit collection; actual parsed packet coverage proves resolution and quality. Temporary index/header memory is admitted under the existing metadata budget. In the hash-pinned virtual14GBdiagnostic (TLM replaced by valid COM only in HTTP responses), a512pixel preview now succeeds at12GETs/1262592bytes, where m414failed after4355GETs/21214208bytes. Six new-case runs had357msmedian; no failed-request latency speedup percentage or organizational claim is made. The physical source file and pixel payloads remain unchanged. Coarser256pixel progressive previews are expected before512pixel finals.\n')
if '(4..=128).contains(&w)' in (build/'source/crates/river-server/src/decoder/reduced_tiles.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nTiny projected native tiles: compatible compact resolution views with4through128pixel native tiles can use existing bounded OpenJPEG row groups when at least two CPU workers are leased. The shared worker budget still applies; RIVER_OPENJPEG_THREADS_PER_DECODE=1 retains synchronous generic decoding. Compressed source selection and pixel payloads do not change. On the fixed generated14GBpreview, two six-pair m422/m436 runs measured389to240ms and385to194ms medians at identical12GETs/1262592bytes. Four-client16CPUcohorts measured569to389ms but used about29percent more CPU per cohort. This is a latency/CPU tradeoff, not free CPU or organizational qualification. Independent16bit grayscale/RGB,quality and clipped-window checks passed. Read PERFORMANCE.json for exact scope and pending qualification; stable/LTS pins remain unchanged.\n')
if 'fn payload_bootstrap_indices' in (build/'source/crates/river-server/src/remote_source.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nCold payload learning: in adaptive mode, a source with measured GET latency of at least2ms and insufficient observations can fetch a subset of already-required ranges before planning the rest. Three qualifying throughput samples and the configured minimumSamples are required; no probe requests are added. Nearby sources, insufficient batches, tight GET quotas and selections exceeding the8MiBpin budget retain the existing path. Pin accounting counts selected blocks only. Existing PUT /api/settings/read-policy controls mode,minimumSamples,maxSourceRequests(concurrency),maxOverfetchRatio and serialTransport; settings persist. Benchmarks used adaptive/minimumSamples8/sourcecap4/overfetch2/SDK. At20msGETlatency the frozen50MBdeep tile used16GETs/4781056bytes instead of27GETs/3459072bytes. At1MiB/s,extraBytesCostMore retained the original IO. The lastDecision describes the remaining batch after required learning reads; use total source diagnostics/origin counters for all phases. RIVER_S3_REQUEST_RANGES remains a per-context attempt cap, not an aggregate HTTP-request or RPS limit. Read PERFORMANCE.json for all samples, network conditions, noisy nearby cases and current qualification. Do not replace existing S3 connections or persisted settings during upgrade.\n')
if 'refresh_payload_costs:Option<bool>' in (build/'source/crates/river-server/src/read_policy.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nOptional mature-source refresh: refreshPayloadCosts=true in PUT /api/settings/read-policy enables a small already-required throughput read before a learned plan would overfetch. Default off. RIVER_S3_REFRESH_PAYLOAD_COSTS=true supplies the ENVdefault; explicit persisted API values override it. Whole configuration export/import includes the effective value. The current valid body-rate measurement caps the smoothed estimate. In six paired variable-source sequences, the opt-in setting reduced5.12sto3.60sand5.10MBto3.46MBafter a sudden bandwidth drop; GETs increased10to27 to avoid excess bytes. Unconditional refresh slowed steady fast requests277to319ms in the earlier prototype,so it is not enabled by default. The revised default-off configuration retained10GETs/5.10MBon a mature steady fast source. Nearby three-size correctness and repeatIOchecks passed. This is a feature-configuration result,not a universal speedup or a replacement of stable458. Preserve existing S3 sources/settings; enable only when the latency/bandwidth tradeoff is desired.\n')
if 'learned_header_probe' in (build/'source/crates/river-server/src/remote_source/multipart_prefix.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nAdaptive multipart header reads: multilayer compact previews can use a previously validated header size in the same image preparation to bound later header probes between2KiB and16KiB, when measured source latency is at least5ms and overfetch is permitted. Existing byte, pin and request budgets and validation remain in force. Source network learning is shared across requests and images by configured S3 sourceID; header-size learning is image-local. Learning is process-local and resets on restart or connection replacement. Separate sourceIDs learn independently even at the same endpoint. Adaptive applies measured tradeoffs; Observe only measures. PERFORMANCE.json states qualification and limitations; shaped300MB/s results are not measured organizationalS3 performance.\n')
if 'fn learned_shard_count' in (build/'source/crates/river-server/src/remote_source/index_shards.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nAdaptive index seed geometry: eligible whole-image previews can choose their seed count from already validated nontrivial part sizes, the existing speculative-byte budget and available source capacity. Measured source latency must be at least5ms. The16MiBprobe ceiling, bandwidth guard, offset-chain validation and source admission remain active; partial views and explicit shard overrides retain their behavior. Read ORGANIZATIONAL-S3-PROFILE.md for exact tested settings and source-learning limits. The standard startup fallback remains Observe; saved API settings override it. PERFORMANCE.json identifies the measured gain and regression investigations.\n')
if ('discovery_reuse_confident' in (build/'source/crates/river-server/src/read_policy.rs').read_text(encoding='utf-8')
        and 'fn accept_probe' in (build/'source/crates/river-server/src/remote_source/index_shards.rs').read_text(encoding='utf-8')):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nLearned-source discovery: Adaptive mode can reuse a mature source profile after fresh confirming preparation reads and validated recent part geometry. Fresh sources and contradictory opening timing retain full calibration. The mandatory bandwidth pilot refreshes latency before committing speculative traffic, so an ending startup delay cannot authorize a large read using stale timing. Learning remains process-local and keyed by configured sourceID. Read ORGANIZATIONAL-S3-PROFILE.md and PERFORMANCE.json for settings, exact qualification and limits.\n')
if '/api/tiles/{image}' in (build/'source/crates/river-server/src/main.rs').read_text(encoding='utf-8'):
    with (package/'README.md').open('a',encoding='utf-8') as readme:
        readme.write('\nShared streamer contract: GET /api/tiles/{encoded-original-image-url} and default streamer tiles use the same256pixel,ratio1 pixel pyramid. maxzoom=max(0,ceil(log2(max(width,height)/256))). PNG/WebP and transparent fixed-size padding work through connected sources without client-side registration. Explicit geometry and WMTS retain their behavior. Read STREAMER-COMPATIBILITY.md before replacing an external tile-info service: its pyramid policy must match. Local CORS defaults include Crystal at127.0.0.1:19074 and localhost:19074; configure existing RIVER_CORS_ORIGINS for other browser origins or use a same-origin proxy.\n')
if performance is not None and json.loads(performance.read_text(encoding='utf-8')).get('qualifiedForPromotion') is False:
    readme_path=package/'README.md'
    readme_path.write_text('EXPERIMENTAL CANDIDATE: NOT RELEASE-QUALIFIED\n\n'
        'Read PERFORMANCE.json for the exact current evidence and pending checks. '
        'Feature descriptions below include inherited behavior and historical validation; '
        'they do not establish qualification of this candidate. Stable/LTS are unchanged.\n\n'
        +readme_path.read_text(encoding='utf-8'),encoding='utf-8')
if manifest['releaseChannels']:
    readme_path = package / 'README.md'
    readme_path.write_text('CURRENT RELEASE CHANNELS: ' + ', '.join(manifest['releaseChannels']) + '\n'
        'Read release.json and PERFORMANCE.json for current qualification and limitations. '
        'Inherited checkpoint notes below describe their historical validation; they do not override current channel metadata.\n\n'
        + readme_path.read_text(encoding='utf-8'), encoding='utf-8')
files = sorted(p for p in package.rglob('*') if p.is_file() and p.name != 'SHA256SUMS')
(package / 'SHA256SUMS').write_text(''.join(hashlib.sha256(p.read_bytes()).hexdigest() + '  ' + p.relative_to(package).as_posix() + '\n' for p in files), encoding='ascii', newline='\n')
if args.stage_only:
    print(json.dumps(dict(prepared=str(package),imageId=status['imageId'],archiveSha256=status['archiveSha256'])))
    raise SystemExit(0)
bundle = Path(str(package) + '.zip')

with zipfile.ZipFile(bundle, 'w', compression=zipfile.ZIP_STORED) as target:
    for path in sorted(package.rglob('*')):
        if path.is_file():
            target.write(path, package.name + '/' + path.relative_to(package).as_posix(),
                         compress_type=zipfile.ZIP_STORED if path.suffix == '.gz' else zipfile.ZIP_DEFLATED,
                         compresslevel=9)
with zipfile.ZipFile(bundle) as archive:
    assert archive.testzip() is None, 'Deployment ZIP failed its CRC check'
    prefix = package.name + '/'
    checksums = archive.read(prefix + 'SHA256SUMS').decode('ascii').splitlines()
    for line in checksums:
        expected, name = line.split('  ', 1)
        assert hashlib.sha256(archive.read(prefix + name)).hexdigest() == expected, name
digest = hashlib.sha256(bundle.read_bytes()).hexdigest()
Path(str(bundle) + '.sha256').write_text(digest + '  ' + bundle.name + '\n', encoding='ascii', newline='\n')
print(json.dumps(dict(bundle=str(bundle), bytes=bundle.stat().st_size, sha256=digest,
                     verifiedFiles=len(checksums), zipCrc=True)))

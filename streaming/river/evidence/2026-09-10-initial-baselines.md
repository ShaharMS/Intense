# River initial baselines — 2026-09-10

## Environment

- Windows 11 Pro 64-bit (`10.0.26200`), Intel Core Ultra 9 185H (16 cores / 22 logical processors), 23.37 GiB RAM.
- FFmpeg `8.1.2-full_build-www.gyan.dev`, built with `--enable-libopenjpeg` and `--enable-libwebp`.
- River Rust workspace: `cargo test --workspace` passed before the runs.

## Corpus

The downloaded corpus is pinned in [`../corpus/manifest.json`](../corpus/manifest.json). Cached asset bytes are intentionally ignored by Git; their SHA-256 identities make them reproducible.

## OpenJPEG full-decode baseline

Each value is ten warm runs of `ffmpeg -hide_banner -loglevel error -threads 1 -i <asset> -frames:v 1 -f null -`. The reported p95 is the nearest-rank p95 of those ten elapsed wall-clock samples. This is a full-image decoder baseline through FFmpeg/OpenJPEG, not a River request.

| Asset | Dimensions | Decoded pixel format | Median | p95 |
| --- | ---: | --- | ---: | ---: |
| `Bretagne2.j2k` | 2592×1944 | rgb24 | 1080.456 ms | 1089.384 ms |
| `small_world_non_consecutive_tilepart_tlm.jp2` | 400×200 | rgb24 | 55.704 ms | 58.098 ms |
| `issue559-eci-090-CIELab.jp2` | 424×212 | rgb48le | 67.939 ms | 69.735 ms |

The 5 MP source confirms why River must obtain source-region/layer decoding before it can pursue the 250 ms whole-viewport preview target. A full one-thread decode alone exceeds that budget by about 4.4×.

## River protocol-fixture baseline

Ten concurrent Node 22 WebSocket clients requested the fixture asset at 1920×1080, zoom 0. Each waited for all 40 preview tile frames (256-pixel grid). The fixture sends a tiny static WebP and does **no** JPEG 2000 decode, WebP decode, WebGL upload, MinIO access, or OTLP collector export.

- Median full-preview framing/transport time: **39.128 ms**
- p95 full-preview framing/transport time: **49.006 ms**
- Range: **23.572–49.006 ms**
- Each client received 120 total tile frames (preview, gain, final), and all frames carried the accepted non-zero OTLP trace ID.

This clears the protocol-only portion of the 250 ms target. It does not validate River’s product goal; the direct OpenJPEG adapter, ROI decode, WebP encode, browser decode/upload, and real cache measurements remain unimplemented.

## Operational limitation

Docker Desktop's Linux engine was stopped during this run. The Compose configuration validates, but MinIO and the OTLP collector/Jaeger/Grafana stack were not started; local exporter connection failures are expected in this state.

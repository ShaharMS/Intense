# Streamer compatibility contract

The shared River compatibility implementation serves PNG and WebP from original
image URLs belonging to exactly one connected S3 source. Clients do not need
asset registration, River manifest IDs, geometry injection or bucket scans.
A source must already be configured on the selected River instance. Merely
supplying an arbitrary HTTP URL does not authorize River to fetch it.

Use these two endpoints from the same River deployment:

- `GET /api/tiles/{percent-encoded-original-image-URL}`
- `GET /streamer/v1/{z}/{x}/{y}/tile.png?image={percent-encoded-original-image-URL}&edge=transparent`

The tile suffix may be `tile.webp`. Encode the complete original URL as one
path segment or one query value, including any existing percent signs, plus,
ampersand and query delimiters. River decodes the outer URL encoding once;
object-key escapes remain part of the original URL until source resolution.

Tile-info returns `bounds:[0,height,width,0]`, `minzoom:0` and `maxzoom:N`.
The default compatibility pyramid uses tiles of256pixels at pixel ratio1,
where `N=max(0,ceil(log2(max(width,height)/256)))`. Zoom increases toward full
resolution. At zoom z, a tile covers `256*2^(N-z)` original source pixels on
each axis. One tile at z0 covers the image; padding accounts for unused space.
For40980x10980, N is8; for4096x4096 it is4; for65536x163840 it is10. These are
pixel bounds, not geographic coordinates or a geographic overlay policy.

The image's JPEG2000 resolution count does not define this external pyramid.
At coarse zooms beyond native reductions, River decodes a supported reduction
then resamples to the requested bitmap dimensions. With edge=transparent,
tiles are always256x256 and the area outside the image has alpha0. Crop mode
keeps visible dimensions; black padding has opaque black outside the image.
PNG is lossless. WebP remains River's lossy quality82 encoding.

All existing explicit geometry overrides remain available: tileSize and maxZoom
are required together when overriding geometry; minZoom defaults to0 and
tilePixelRatio to1. WMTS keeps its established512pixel/codestream-level grid.
Clients relying on the old default streamer WMTS-style pyramid should use WMTS
or specify their old geometry explicitly. Default compatibility geometry supports
image dimensions through2^31 pixels; larger grids require explicit multi-root
geometry and remain subject to decoder/request limits.

An external tile-info service is compatible only if it uses these same bounds,
zoom,256pixel size and ratio1 conventions. River cannot infer a different
service's policy from the image URL. Crystal can use River for both tile-info
and pixel tiles through its existing URL configuration. Search, metadata,
geographic placement and grids remain external services.

The local `river-lts` endpoint is http://127.0.0.1:18082. All channels use the
same CORS policy: by default any browser hosting origin works, including
credentialed requests. River echoes the request origin, preflight method and
requested header names; it never combines wildcard origins with credentials.
Every response exposes its actual headers, including raster metadata, pagination
cursors and error request IDs. Preflights are cached for600seconds and support
private-network request headers. HTTP OPTIONS does not contact S3.

`RIVER_CORS_ORIGINS=*` (also the unset default) accepts any origin. To restrict
browser access, set a comma-separated list of complete HTTP/HTTPS origins.
Local Compose accepts the global `RIVER_CORS_ORIGINS` value and per-slot
`RIVER_<SLOT>_CORS_ORIGINS` overrides. CORS is a
browser policy, not authentication. WebSockets use ws/wss and the proxy must
forward upgrades. An HTTPS browser page needs an HTTPS/WSS River endpoint;
browser mixed-content or local-network permission blocks cannot be removed by
CORS headers. Reverse proxies should forward River's CORS headers rather than
add a second Access-Control-Allow-Origin header.

Source edits alone do not establish deployed availability. Check deploy/releases.json,
the selected container imageID, /openapi.json and a real tile-info/tile response
before treating a channel as updated. Shared channel aliases may point to the
same qualified image; superseded incompatible Docker images are retired.

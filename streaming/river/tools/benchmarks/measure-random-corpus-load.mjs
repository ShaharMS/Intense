const viewers = Number.parseInt(process.env.RIVER_VIEWERS ?? '10', 10);
const screenWidth = Number.parseInt(process.env.RIVER_SCREEN_WIDTH ?? '1920', 10);
const screenHeight = Number.parseInt(process.env.RIVER_SCREEN_HEIGHT ?? '1080', 10);
const requestedZoom = Number.parseInt(process.env.RIVER_ZOOM ?? '3', 10);
const server = process.env.RIVER_HTTP_URL ?? 'http://127.0.0.1:18080';
const wsServer = process.env.RIVER_WS_URL ?? server.replace(/^http/, 'ws') + '/ws';
const seed = Number.parseInt(process.env.RIVER_SEED ?? '20260914', 10) >>> 0;
const timeoutMs = Number.parseInt(process.env.RIVER_TIMEOUT_MS ?? '30000', 10);
const targetP95Ms = Number.parseFloat(process.env.RIVER_TARGET_P95_MS ?? '250');
const completionPass = process.env.RIVER_COMPLETION_PASS ?? 'first';
const selectedAssetId = process.env.RIVER_ASSET_ID;

function randomGenerator(initialSeed) {
  let state = initialSeed;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4_294_967_296;
  };
}

function viewportFor(manifest, random) {
  const zoom = Math.max(0, Math.min(requestedZoom, manifest.resolutionLevels - 1));
  const scale = 2 ** zoom;
  const width = Math.min(manifest.width, screenWidth * scale);
  const height = Math.min(manifest.height, screenHeight * scale);
  const x = Math.floor(random() * (manifest.width - width + 1));
  const y = Math.floor(random() * (manifest.height - height + 1));
  const sourceTile = 512 * scale;
  const frames = (Math.floor((x + width - 1) / sourceTile) - Math.floor(x / sourceTile) + 1)
    * (Math.floor((y + height - 1) / sourceTile) - Math.floor(y / sourceTile) + 1);
  return { x, y, width, height, zoom, frames };
}

const assetsResponse = await fetch(`${server}/api/assets?refresh=true`);
if (!assetsResponse.ok) throw new Error(`asset list returned HTTP ${assetsResponse.status}`);
const allAssets = await assetsResponse.json();
const fastAssets = allAssets.filter((asset) => asset.capability?.status === 'fastPath'
  && (!selectedAssetId || asset.assetId === selectedAssetId));
if (!fastAssets.length) throw new Error('the River corpus has no fast-path JPEG 2000 assets');

const random = randomGenerator(seed);
const assignments = Array.from({ length: viewers }, (_, index) => {
  // Cycle through a randomized corpus ordering before reusing an image.
  const asset = fastAssets[Math.floor(random() * fastAssets.length)];
  return { index, asset, viewport: viewportFor(asset, random) };
});

const samples = [];
await Promise.all(assignments.map(({ index, asset, viewport }) => new Promise((resolve, reject) => {
  const socket = new WebSocket(wsServer);
  const started = performance.now();
  let receivedPreviewFrames = 0;
  let trackedPass = completionPass === 'first' ? null : completionPass;
  let settled = false;
  const finish = (error) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    socket.close();
    error ? reject(error) : resolve();
  };
  const timer = setTimeout(() => finish(new Error(`viewer ${index} timed out after ${timeoutMs} ms`)), timeoutMs);

  socket.addEventListener('open', () => socket.send(JSON.stringify({
    type: 'viewport',
    viewport: {
      assetId: asset.assetId,
      sequence: index + 1,
      x: viewport.x,
      y: viewport.y,
      width: viewport.width,
      height: viewport.height,
      zoom: viewport.zoom,
    },
  })));
  socket.addEventListener('error', () => finish(new Error(`viewer ${index} websocket failed`)));
  socket.addEventListener('message', async (event) => {
    if (typeof event.data === 'string') {
      const control = JSON.parse(event.data);
      if (control.type === 'error') finish(new Error(`viewer ${index}: ${control.code}: ${control.message}`));
      return;
    }
    const bytes = Buffer.from(await event.data.arrayBuffer());
    if (bytes.subarray(0, 4).toString('utf8') !== 'RIVR') return finish(new Error('received an unknown binary frame'));
    const headerLength = bytes.readUInt32LE(4);
    const header = JSON.parse(bytes.subarray(8, 8 + headerLength).toString('utf8'));
    if (header.sequence !== index + 1) return;
    // A JP2 with one quality layer begins at `final`; layered sources begin
    // at `preview`. Default to complete coverage of whichever pass arrives
    // first, or wait for a named progressive pass such as `gain`.
    trackedPass ??= header.pass;
    if (header.pass !== trackedPass) return;
    receivedPreviewFrames += 1;
    if (receivedPreviewFrames === viewport.frames) {
      samples.push({
        elapsedMs: performance.now() - started,
        serverElapsedMs: header.serverTiming.elapsedMs,
        decoderRoute: header.serverTiming.decoderRoute ?? 'missing',
      });
      finish();
    }
  });
})));

const sorted = samples.map((sample) => sample.elapsedMs).sort((left, right) => left - right);
const p95Index = Math.ceil(sorted.length * 0.95) - 1;
const summary = (values) => ({
  medianMs: Number(values[Math.floor((values.length - 1) / 2)].toFixed(3)),
  p95Ms: Number(values[p95Index].toFixed(3)),
  minMs: Number(values[0].toFixed(3)),
  maxMs: Number(values.at(-1).toFixed(3)),
});
const result = {
  benchmark: `river-random-jpeg2000-corpus-${completionPass === 'first' ? 'first-pass' : completionPass}`,
  server,
  viewers,
  corpusAssetsAvailable: fastAssets.length,
  distinctAssetsExercised: new Set(assignments.map(({ asset }) => asset.assetId)).size,
  screenViewport: { width: screenWidth, height: screenHeight, requestedZoom },
  seed,
  selectedAssetId: selectedAssetId ?? null,
  targetP95Ms,
  completionPass,
  timeToFullFirstPass: summary(sorted),
  lastFrameServerElapsed: summary(samples.map((sample) => sample.serverElapsedMs).sort((left, right) => left - right)),
  decoderRoutesObserved: [...new Set(samples.map((sample) => sample.decoderRoute))].sort(),
  limitations: ['Measures protocol delivery only: no browser WebP decode or WebGL upload', 'Uses the locally registered JPEG 2000 corpus only'],
};
console.log(JSON.stringify(result));
if (!Number.isFinite(targetP95Ms) || result.timeToFullFirstPass.p95Ms > targetP95Ms) {
  console.error(`FAIL: first-pass p95 ${result.timeToFullFirstPass.p95Ms} ms exceeds ${targetP95Ms} ms`);
  process.exitCode = 1;
}

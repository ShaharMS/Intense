const server = process.env.RIVER_HTTP_URL ?? 'http://127.0.0.1:18080';
const wsServer = process.env.RIVER_WS_URL ?? server.replace(/^http/, 'ws') + '/ws';
const updates = Number.parseInt(process.env.RIVER_CHURN_UPDATES ?? '12', 10);
const intervalMs = Number.parseInt(process.env.RIVER_CHURN_INTERVAL_MS ?? '60', 10);
const targetMs = Number.parseFloat(process.env.RIVER_CHURN_TARGET_MS ?? '250');
const timeoutMs = Number.parseInt(process.env.RIVER_TIMEOUT_MS ?? '10000', 10);

const assetsResponse = await fetch(`${server}/api/assets`);
if (!assetsResponse.ok) throw new Error(`asset list returned HTTP ${assetsResponse.status}`);
const assets = await assetsResponse.json();
const asset = assets.find((candidate) => candidate.assetId === process.env.RIVER_ASSET_ID)
  ?? assets.find((candidate) => candidate.width > 10_000 && candidate.capability?.status === 'fastPath');
if (!asset) throw new Error('no fast-path JPEG 2000 corpus asset is available for churn testing');

const zoom = Math.min(3, asset.resolutionLevels - 1);
const scale = 2 ** zoom;
const width = Math.min(asset.width, 1920 * scale);
const height = Math.min(asset.height, 1080 * scale);
const sourceTile = 512 * scale;
const framesFor = (x, y) => (
  (Math.floor((x + width - 1) / sourceTile) - Math.floor(x / sourceTile) + 1)
  * (Math.floor((y + height - 1) / sourceTile) - Math.floor(y / sourceTile) + 1)
);

const result = await new Promise((resolve, reject) => {
  const socket = new WebSocket(wsServer);
  let sequence = 0;
  let lastViewport = null;
  let lastSentAt = 0;
  let received = new Set();
  let pass = null;
  let finalInputSent = false;
  let timer;
  const fail = (error) => {
    clearTimeout(timer);
    socket.close();
    reject(error);
  };
  const sendNext = () => {
    sequence += 1;
    const maxX = Math.max(0, asset.width - width);
    const maxY = Math.max(0, asset.height - height);
    // Deterministic diagonal motion exercises same-zoom tile repositioning,
    // the exact interaction the viewer produces while dragging.
    const fraction = updates === 1 ? 0 : (sequence - 1) / (updates - 1);
    lastViewport = {
      assetId: asset.assetId,
      sequence,
      x: Math.round(maxX * fraction),
      y: Math.round(maxY * (1 - fraction)),
      width,
      height,
      zoom,
    };
    lastSentAt = performance.now();
    received = new Set();
    pass = null;
    socket.send(JSON.stringify({ type: 'viewport', viewport: lastViewport }));
    if (sequence < updates) setTimeout(sendNext, intervalMs);
    else finalInputSent = true;
  };
  socket.addEventListener('open', () => {
    timer = setTimeout(() => fail(new Error(`churn test timed out after ${timeoutMs} ms`)), timeoutMs);
    sendNext();
  });
  socket.addEventListener('error', () => fail(new Error('churn test WebSocket failed')));
  socket.addEventListener('message', async (event) => {
    if (typeof event.data === 'string') {
      const control = JSON.parse(event.data);
      if (control.type === 'error') fail(new Error(`${control.code}: ${control.message}`));
      return;
    }
    const bytes = Buffer.from(await event.data.arrayBuffer());
    if (bytes.subarray(0, 4).toString('utf8') !== 'RIVR') return;
    const headerLength = bytes.readUInt32LE(4);
    const header = JSON.parse(bytes.subarray(8, 8 + headerLength).toString('utf8'));
    if (!finalInputSent || !lastViewport || header.sequence !== lastViewport.sequence) return;
    pass ??= header.pass;
    if (header.pass !== pass) return;
    received.add(`${header.tileX}:${header.tileY}`);
    if (received.size !== framesFor(lastViewport.x, lastViewport.y)) return;
    clearTimeout(timer);
    const elapsedMs = performance.now() - lastSentAt;
    socket.close();
    resolve({ elapsedMs, frames: received.size, finalSequence: lastViewport.sequence });
  });
});

const output = {
  benchmark: 'river-interactive-jpeg2000-churn',
  assetId: asset.assetId,
  updates,
  updateIntervalMs: intervalMs,
  targetMs,
  finalViewport: { width, height, zoom },
  firstPassAfterFinalInputMs: Number(result.elapsedMs.toFixed(3)),
  frames: result.frames,
  finalSequence: result.finalSequence,
};
console.log(JSON.stringify(output));
if (!Number.isFinite(targetMs) || result.elapsedMs > targetMs) {
  console.error(`FAIL: final interactive first pass ${result.elapsedMs.toFixed(3)} ms exceeds ${targetMs} ms`);
  process.exitCode = 1;
}

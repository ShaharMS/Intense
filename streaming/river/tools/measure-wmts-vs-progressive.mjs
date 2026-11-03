import { mkdir, writeFile } from 'node:fs/promises';
import { performance } from 'node:perf_hooks';

const api = process.env.RIVER_API ?? 'http://127.0.0.1:18080';
const assetId = process.env.RIVER_ASSET_ID ?? '94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290';
const rounds = Number(process.env.RIVER_WMTS_ROUNDS ?? 5);
const viewport = { assetId, sequence: 1, x: 0, y: 0, width: 512, height: 512, zoom: 0 };
const manifestResponse = await fetch(`${api}/api/assets/${assetId}/manifest`);
if (!manifestResponse.ok) throw new Error(`manifest returned HTTP ${manifestResponse.status}`);
const manifest = await manifestResponse.json();
const matrix = manifest.resolutionLevels - 1;
const wmtsUrl = new URL(`${api}/wmts`);
for (const [key, value] of Object.entries({ SERVICE: 'WMTS', REQUEST: 'GetTile', VERSION: '1.0.0', LAYER: assetId, STYLE: 'default', FORMAT: 'image/png', TILEMATRIXSET: `RiverPixel-${assetId}`, TILEMATRIX: matrix, TILEROW: 0, TILECOL: 0 })) wmtsUrl.searchParams.set(key, value);

async function measureWmts() {
  const started = performance.now();
  const response = await fetch(wmtsUrl);
  if (!response.ok) throw new Error(`WMTS GetTile returned HTTP ${response.status}`);
  const bytes = await response.arrayBuffer();
  if (response.headers.get('content-type') !== 'image/png') throw new Error('WMTS GetTile did not return image/png');
  if (new Uint8Array(bytes).slice(0, 8).join(',') !== '137,80,78,71,13,10,26,10') throw new Error('WMTS GetTile returned an invalid PNG payload');
  return { elapsedMs: performance.now() - started, contentType: response.headers.get('content-type'), bytes: bytes.byteLength, traceId: response.headers.get('x-river-trace-id'), decoderRoute: response.headers.get('x-river-decoder-route'), queueMs: Number(response.headers.get('x-river-queue-ms')), decodeMs: Number(response.headers.get('x-river-decode-ms')), encodeMs: Number(response.headers.get('x-river-encode-ms')), serverMs: Number(response.headers.get('x-river-server-ms')) };
}

async function measureProgressive() {
  const socketUrl = new URL(`${api}/ws`);
  socketUrl.protocol = socketUrl.protocol === 'https:' ? 'wss:' : 'ws:';
  const socket = new WebSocket(socketUrl);
  const started = performance.now();
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { socket.close(); reject(new Error('progressive request timed out')); }, 120_000);
    socket.onerror = () => { clearTimeout(timeout); reject(new Error('progressive WebSocket failed')); };
    socket.onopen = () => socket.send(JSON.stringify({ type: 'viewport', viewport }));
    socket.onmessage = async (event) => {
      if (typeof event.data === 'string') {
        const control = JSON.parse(event.data);
        if (control.type === 'error') { clearTimeout(timeout); socket.close(); reject(new Error(control.message)); }
        return;
      }
      const frame = Buffer.from(await event.data.arrayBuffer());
      const headerLength = frame.readUInt32LE(4);
      const header = JSON.parse(frame.subarray(8, 8 + headerLength).toString());
      if (header.contentType !== 'image/webp') { clearTimeout(timeout); socket.close(); reject(new Error('progressive WebSocket did not return image/webp')); return; }
      if (header.pass !== 'final' || header.tileX !== 0 || header.tileY !== 0) return;
      clearTimeout(timeout);
      socket.close();
      resolve({ elapsedMs: performance.now() - started, contentType: header.contentType, bytes: frame.length - 8 - headerLength, traceId: header.traceId, decoderRoute: header.serverTiming.decoderRoute, queueMs: header.serverTiming.workerQueueMs, decodeMs: header.serverTiming.decodeMs, encodeMs: header.serverTiming.encodeMs, serverMs: header.serverTiming.elapsedMs });
    };
  });
}

// Warm both code paths, then alternate order to reduce order/cache bias.
await measureWmts();
await measureProgressive();
const samples = [];
for (let index = 0; index < rounds; index += 1) {
  const order = index % 2 ? ['progressive', 'wmts'] : ['wmts', 'progressive'];
  const pair = {};
  for (const mode of order) pair[mode] = mode === 'wmts' ? await measureWmts() : await measureProgressive();
  samples.push({ index: index + 1, ...pair });
}
const summarize = (mode) => {
  const values = samples.map((sample) => sample[mode].elapsedMs).sort((a, b) => a - b);
  return { count: values.length, medianMs: values[Math.floor(values.length / 2)], p95Ms: values[Math.ceil(values.length * 0.95) - 1], minMs: values[0], maxMs: values.at(-1) };
};
const report = { at: new Date().toISOString(), api, assetId, workload: { description: 'one 512x512 source-pixel tile at zoom 0; WMTS GetTile versus waiting for the progressive final WebSocket frame', rounds, matrix, tileRow: 0, tileCol: 0, warmupEachMode: 1 }, summary: { wmts: summarize('wmts'), progressive: summarize('progressive') }, samples };
await mkdir('reports', { recursive: true });
const out = `reports/wmts-progressive-${new Date().toISOString().replaceAll(':', '-').replaceAll('.', '-')}.json`;
await writeFile(out, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify({ report: out, ...report.summary }, null, 2));

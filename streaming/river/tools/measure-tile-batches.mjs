import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, writeFile } from 'node:fs/promises';

const server = process.env.RIVER_HTTP_URL ?? 'http://127.0.0.1:18080';
const viewers = Number(process.env.RIVER_VIEWERS ?? 10);
const rounds = Number(process.env.RIVER_ROUNDS ?? 3);
const timeoutMs = Number(process.env.RIVER_TIMEOUT_MS ?? 120000);
const maxRatio = Number(process.env.RIVER_BATCH_MAX_RATIO ?? 1.2);
const profile = process.env.RIVER_BATCH_PROFILE ?? 'sample';
const baselineOnly = process.env.RIVER_BATCH_BASELINE === '1';
const seed = Number(process.env.RIVER_SEED ?? 20260915) >>> 0;
assert(viewers > 0 && rounds > 0 && maxRatio > 0);
const assetResponse = await fetch(`${server}/api/assets?refresh=true`);
assert(assetResponse.ok);
const assets = await assetResponse.json();
const sample = assets.find(a => a.assetId === '94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290');
assert(sample);
const corpus = assets.filter(a => a.assetId !== sample.assetId && a.capability.status === 'fastPath');
let sequence = 0;
const key = tile => `${tile.z}/${tile.x}/${tile.y}`;
const frameKey = h => `${h.zoom}/${h.tileX}/${h.tileY}`;
const batchMessage = (asset, tiles) => ({ type: 'tiles', batch: { assetId: asset.assetId, sequence: ++sequence, tiles } });
const requestSequence = message => (message.batch ?? message.viewport)?.sequence;

function viewportTiles(v) {
  const edge = 512 * 2 ** v.zoom;
  const tiles = [];
  for (let y = Math.floor(v.y / edge); y <= Math.floor((v.y + v.height - 1) / edge); y++) {
    for (let x = Math.floor(v.x / edge); x <= Math.floor((v.x + v.width - 1) / edge); x++) tiles.push({ x, y, z: v.zoom });
  }
  return tiles;
}

function collect(path, message, expectedTiles, { pass = 'final', errorCode, churn = [] } = {}) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(server.replace(/^http/, 'ws') + path);
    socket.binaryType = 'arraybuffer';
    const expected = new Set(expectedTiles.map(key));
    const frames = new Map();
    const coverage = new Map();
    const routes = new Set();
    let accepted;
    let start;
    let previewMs;
    let finished = false;
    const finish = (error, result) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      socket.close();
      error ? reject(error) : resolve(result);
    };
    const timer = setTimeout(() => finish(new Error(`${path} sequence ${requestSequence(message)} timed out`)), timeoutMs);
    socket.addEventListener('open', () => {
      start = performance.now();
      for (const previous of churn) socket.send(JSON.stringify(previous));
      socket.send(typeof message === 'string' ? message : JSON.stringify(message));
    });
    socket.addEventListener('error', () => finish(new Error('WebSocket error')));
    socket.addEventListener('close', () => { if (!finished) finish(new Error('Socket closed before coverage')); });
    socket.addEventListener('message', event => {
      if (finished) return;
      try {
        if (typeof event.data === 'string') {
          const control = JSON.parse(event.data);
          if (typeof message !== 'string' && errorCode !== 'invalid_message' && control.sequence !== requestSequence(message)) return;
          if (errorCode) {
            assert.equal(control.type, 'error', 'invalid input must fail before acceptance');
            assert.equal(control.code, errorCode);
            return finish(null, control);
          }
          assert.notEqual(control.type, 'error', JSON.stringify(control));
          assert.notEqual(control.type, 'limited', JSON.stringify(control));
          assert.equal(control.type, 'accepted');
          assert.match(control.traceId, /^[a-f0-9]{32}$/);
          assert.notEqual(control.traceId, '0'.repeat(32));
          accepted = control;
          return;
        }
        assert(!errorCode, 'invalid batch emitted a tile');
        const bytes = Buffer.from(event.data);
        assert.equal(bytes.subarray(0, 4).toString(), 'RIVR');
        const length = bytes.readUInt32LE(4);
        assert(length > 0 && 8 + length < bytes.length);
        const h = JSON.parse(bytes.subarray(8, 8 + length).toString());
        if (h.sequence !== requestSequence(message)) return;
        assert(accepted, 'frame arrived before acceptance');
        assert.equal(h.traceId, accepted.traceId);
        assert.equal(h.sessionId, accepted.sessionId);
        assert.equal(h.assetId, (message.batch ?? message.viewport).assetId);
        assert.equal(h.contentType, 'image/webp');
        assert(h.outputWidth > 0 && h.outputWidth <= 512 && h.outputHeight > 0 && h.outputHeight <= 512);
        const payload = bytes.subarray(8 + length);
        assert.equal(payload.subarray(0, 4).toString(), 'RIFF');
        assert.equal(payload.subarray(8, 12).toString(), 'WEBP');
        const id = `${h.pass}/${frameKey(h)}`;
        assert(!frames.has(id), `duplicate frame ${id}`);
        frames.set(id, {
          hash: createHash('sha256').update(payload).digest('hex'),
          width: h.outputWidth, height: h.outputHeight, layers: h.sourceLayers,
        });
        routes.add(h.serverTiming.decoderRoute);
        for (const field of ['workerQueueMs', 'decodeMs', 'encodeMs', 'elapsedMs']) assert(Number.isFinite(h.serverTiming[field]));
        if (!coverage.has(h.pass)) coverage.set(h.pass, new Set());
        coverage.get(h.pass).add(frameKey(h));
        const complete = [...expected].every(k => coverage.get(h.pass).has(k));
        if (h.pass === 'preview' && complete) previewMs = performance.now() - start;
        if (h.pass !== 'preview') assert(previewMs !== undefined, 'refinement started before full preview coverage');
        if (h.pass === pass && complete) finish(null, {
          elapsedMs: performance.now() - start, previewMs, frames: Object.fromEntries([...frames].sort()),
          routes: [...routes], traceId: accepted.traceId,
        });
      } catch (error) { finish(error); }
    });
  });
}

const checks = [];
if (!baselineOnly) {
  // Exact payload parity through every pass, including clipped right/bottom tiles.
  for (const v of [
    { x: 0, y: 0, width: 1920, height: 1080, zoom: 0 },
    { x: 2300, y: 1750, width: 292, height: 194, zoom: 0 },
    { x: 0, y: 0, width: sample.width, height: sample.height, zoom: 1 },
  ]) {
    const tiles = viewportTiles(v);
    const viewport = { type: 'viewport', viewport: { assetId: sample.assetId, sequence: ++sequence, ...v } };
    const old = await collect('/ws', viewport, tiles);
    const batch = await collect('/ws/tiles', batchMessage(sample, tiles.toReversed()), tiles);
    assert.deepEqual(batch.frames, old.frames);
  }
  checks.push('Byte-identical WebP and output metadata across all passes for HD, edges, and zoom 1');
  const sparse = [{x:0,y:0,z:0}, {x:1,y:1,z:0}, {x:5,y:3,z:0}, {x:0,y:0,z:1}];
  const expanded = [...sparse, {x:1,y:0,z:0}, {x:0,y:1,z:0}];
  const result = await collect('/ws/tiles', batchMessage(sample, [...sparse.toReversed(), sparse[0]]), expanded);
  const returnedPasses = new Set(Object.keys(result.frames).map(id=>id.split('/')[0]));
  assert(returnedPasses.has('preview') && returnedPasses.has('final'));
  for (const pass of returnedPasses) assert.equal(Object.keys(result.frames).filter(id=>id.startsWith(`${pass}/`)).length, expanded.length);
  checks.push('Sparse, unsorted, duplicate, mixed-zoom batch returned 6 unique tiles per pass from 5 entries');
  await collect('/ws', batchMessage(sample, sparse), expanded);
  checks.push('Existing /ws also accepts the new message');
  for (const tiles of [[], Array(257).fill({x:0,y:0,z:0}), [{x:0,y:0,z:0},{x:999,y:0,z:0}], [{x:0,y:0,z:32}]]) {
    await collect('/ws/tiles', batchMessage(sample, tiles), [], {errorCode:'invalid_tiles'});
  }
  await collect('/ws/tiles', batchMessage({...sample, assetId:'f'.repeat(64)}, [{x:0,y:0,z:0}]), [], {errorCode:'asset_not_found'});
  await collect('/ws/tiles', '{', [], {errorCode:'invalid_message'});
  await collect('/ws/tiles', batchMessage(sample, [{x:-1,y:0,z:0}]), [], {errorCode:'invalid_message'});
  checks.push('Empty, oversized, out-of-bounds, unsupported zoom, unknown asset, malformed and negative inputs rejected');
  const tile = [{x:0,y:0,z:0}];
  await collect('/ws/tiles', batchMessage(sample, Array(256).fill(tile[0])), tile);
  const latest = batchMessage(sample, tile);
  await collect('/ws/tiles', latest, tile, {churn:Array.from({length:24},()=>batchMessage(sample,sparse))});
  checks.push('256-entry boundary and 24 superseding requests completed through final');
  console.error(`Protocol checks passed: ${checks.length}`);
}

assert(['sample', 'sparse', 'native', 'roi'].includes(profile), 'unknown batch profile');
const selected = ['sample', 'sparse'].includes(profile) ? [sample] : corpus;
assert(selected.length, 'corpus profile requires registered mosaic assets');
let randomState = seed;
function random() {
  randomState = (Math.imul(1664525, randomState) + 1013904223) >>> 0;
  return randomState / 2 ** 32;
}
const workloads = Array.from({length:viewers}, (_, index) => {
  const asset = selected[index % selected.length];
  const zoom = profile === 'roi' ? 3 : 0;
  const edge = 512 * 2 ** zoom;
  const width = Math.min(asset.width, (profile === 'native' ? 256 : profile === 'sparse' ? 1024 : 1920) * 2 ** zoom);
  const height = Math.min(asset.height, (profile === 'native' ? 256 : profile === 'sparse' ? 1024 : 1080) * 2 ** zoom);
  const x = Math.floor(random() * (Math.floor((asset.width - width) / edge) + 1)) * edge;
  const y = Math.floor(random() * (Math.floor((asset.height - height) / edge) + 1)) * edge;
  const viewport = {assetId:asset.assetId, x,y,width,height,zoom};
  const tiles = viewportTiles(viewport);
  const requestedTiles = profile === 'sparse' ? [tiles.at(-1), tiles[0], tiles[0]] : tiles.toReversed();
  return {asset, viewport, tiles, requestedTiles};
});
const samples = {viewport:[], batch:[]};
async function wave(mode, record = true) {
  const results = await Promise.all(workloads.map(({asset,viewport,tiles,requestedTiles}) => collect(
    mode === 'batch' ? '/ws/tiles' : '/ws',
    mode === 'batch' ? batchMessage(asset, requestedTiles) : {type:'viewport', viewport:{...viewport,sequence:++sequence}},
    tiles, {pass:'preview'},
  )));
  if (record) samples[mode].push(...results.map(({frames,...timing})=>timing));
  console.error(`${profile} ${mode}: ${results.length} viewers, max ${Math.max(...results.map(r=>r.elapsedMs)).toFixed(1)} ms${record?'':' (warmup)'}`);
}
await wave('viewport', false);
if (!baselineOnly) await wave('batch', false);
for (let round=0; round<rounds; round++) {
  for (const mode of baselineOnly ? ['viewport'] : round%2 ? ['batch','viewport'] : ['viewport','batch']) await wave(mode);
}
function summary(values) {
  const sorted = values.map(s=>s.elapsedMs).sort((a,b)=>a-b);
  return {samples:sorted.length, medianMs:sorted[Math.floor((sorted.length-1)/2)], p95Ms:sorted[Math.ceil(sorted.length*.95)-1], maxMs:sorted.at(-1)};
}
const viewport = summary(samples.viewport);
const batch = baselineOnly ? null : summary(samples.batch);
const ratio = batch ? batch.p95Ms / viewport.p95Ms : null;
const report = {
  benchmark:'river-xyz-batch-parity', at:new Date().toISOString(), profile, server, viewers, rounds, seed,
  distinctAssets:new Set(workloads.map(w=>w.asset.assetId)).size,
  checks, viewport, batch, ratio, maxRatio, passed:baselineOnly || ratio<=maxRatio,
  workloads, samples,
  limitations:['Local WebSocket delivery; excludes browser decode/WebGL and remote storage.', 'Warm OS file cache; no River result cache or generated delivery pyramid.', 'Parity is relative to the current viewport path; not a 250 ms target claim.'],
};
const output = process.env.RIVER_BATCH_REPORT ?? `reports/tile-batch-${profile}-${viewers}${baselineOnly?'-baseline':''}`;
await mkdir(output.substring(0,output.lastIndexOf('/')), {recursive:true});
await writeFile(`${output}.json`, JSON.stringify(report,null,2)+'\n');
await writeFile(`${output}.md`, `# XYZ batch verification\n\n${report.at}\n\nProfile: ${profile}; ${viewers} concurrent viewers; ${rounds} measured waves; ${report.distinctAssets} assets.\n\nViewport p95: ${viewport.p95Ms.toFixed(2)} ms.\n${batch ? `\nBatch p95: ${batch.p95Ms.toFixed(2)} ms. Ratio: ${ratio.toFixed(3)}; gate: ${maxRatio}. ${report.passed?'PASS':'FAIL'}.\n`:''}\n## Protocol checks\n\n${checks.map(c=>`- ${c}`).join('\n')}\n\n## Limits\n\n${report.limitations.map(c=>`- ${c}`).join('\n')}\n`);
console.log(JSON.stringify({profile,viewers,rounds,viewport,batch,ratio,passed:report.passed,report:`${output}.json`}));
if (!report.passed) process.exitCode=1;

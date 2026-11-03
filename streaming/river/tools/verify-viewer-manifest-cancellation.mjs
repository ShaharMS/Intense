import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const source = fs.readFileSync(new URL('../web/viewer.js', import.meta.url), 'utf8');
const start = source.indexOf('async function loadAsset(');
const end = source.indexOf('\nasync function refreshAvailableAssets', start);
const network = [];
const inputs = { asset: { value: 'first' } };
const status = { textContent: '' };
let presented = 0;
const context = vm.createContext({ AbortController, inputs, status, apiBase: '',
  wmtsAbort: null, wmtsGeneration: 0, assetLoadAbort: null, assetLoadGeneration: 0,
  assetManifest: null, activeViewport: null, latestSequence: 0,
  renderer: { clear() {} }, boundaryContext: { clearRect() {} }, boundaryCanvas: { width: 10, height: 10 },
  applyAssetDecoderCapabilities() {}, maxZoom: () => 0,
  viewportAt: () => ({}), renderCatalog() {}, presentViewport() { presented++; }, requestViewport() {},
  fetch(url, { signal }) {
    return new Promise((resolve, reject) => {
      network.push({ url, signal, resolve: assetId => resolve({ ok: true,
        json: async () => ({ assetId, width: 1024, height: 768 }) }) });
      signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true });
    });
  }
});
vm.runInContext(source.slice(start, end), context);
const first = context.loadAsset();
inputs.asset.value = 'second';
const second = context.loadAsset();
assert.equal(network[0].signal.aborted, true);
await first;
assert.equal(presented, 0);
network[1].resolve('second');
await second;
assert.equal(presented, 1);
assert.equal(context.assetManifest.assetId, 'second');
assert.equal(context.assetLoadAbort, null);
inputs.asset.value = 'third';
const third = context.loadAsset();
inputs.asset.value = '';
await context.loadAsset();
await third;
assert.equal(network[2].signal.aborted, true);
assert.equal(presented, 1, 'clearing selection allowed obsolete image presentation');
assert.equal(status.textContent.includes('unavailable'), false);
console.log('PASS: switching or clearing assets cancels stale manifest IO without stale presentation or error');

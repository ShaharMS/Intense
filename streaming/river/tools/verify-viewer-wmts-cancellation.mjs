import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

// Exercise the real scheduler with deliberately stalled network responses.
const source = fs.readFileSync(new URL('../web/viewer.js', import.meta.url), 'utf8');
const start = source.indexOf('function visibleWmtsTile(');
const end = source.indexOf('\nlet requestedVisibleTiles', start);
const network = [];
const context = vm.createContext({ AbortController, DOMException, wmtsConcurrency: 2, fetch(url, { signal }) {
  return new Promise((resolve, reject) => {
    const request = { url, signal, resolve: () => resolve({ ok: true, blob: async () => url }) };
    network.push(request);
    signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true });
  });
} });
vm.runInContext(source.slice(start, end), context);
const tiles = { controller: new AbortController(), requests: new Map(), queue: [], active: 0,
  visible: new Set(['old', 'shared', 'queued']) };
const load = key => context.visibleWmtsTile(tiles, key, key).catch(error => error.name);
const old = load('old');
const shared = load('shared');
const queued = load('queued');
assert.equal(network.length, 2);
// Same-zoom pan: preserve overlap, cancel both active and queued obsolete work.
tiles.visible = new Set(['shared', 'new', 'queued']);
const obsolete = tiles.requests.get('old');
obsolete.controller.abort(); tiles.requests.delete('old');
const cancelledQueue = tiles.requests.get('queued');
cancelledQueue.controller.abort(); tiles.requests.delete('queued');
const replacement = load('queued');
const fresh = load('new');
await new Promise(resolve => setImmediate(resolve));
assert.equal(await old, 'AbortError');
assert.equal(await queued, 'AbortError');
assert.equal(network[1].signal.aborted, false, 'overlapping tile was cancelled');
assert.equal(network[2].url, 'queued', 'obsolete queued request blocked its replacement');
network[2].resolve();
await replacement;
await new Promise(resolve => setImmediate(resolve));
assert.equal(network[3].url, 'new');
tiles.controller.abort();
assert.equal(await shared, 'AbortError');
assert.equal(await fresh, 'AbortError');
await new Promise(resolve => setImmediate(resolve));
assert.equal(tiles.active, 0);
assert.equal(tiles.requests.size, 1, 'only completed reusable response remains');
assert.equal(tiles.requests.get('queued').done, true);
network.length = 0;
context.wmtsConcurrency = 4;
const keys = Array.from({ length: 10 }, (_, i) => `tile-${i}`);
const wider = { controller: new AbortController(), requests: new Map(), queue: [], active: 0, visible: new Set(keys) };
const loads = keys.map(key => context.visibleWmtsTile(wider,key,key).catch(error => error.name));
assert.equal(network.length,4);
assert.equal(wider.active,4);
network[0].resolve(); network[1].resolve();
await new Promise(resolve => setImmediate(resolve));
assert.equal(network.length,6);
assert.equal(wider.active,4);
wider.controller.abort();
await Promise.all(loads);
await new Promise(resolve => setImmediate(resolve));
assert.equal(wider.active,0);
console.log('PASS: obsolete active/queued tiles cancelled; overlap retained; replacement survives cleanup; mode abort frees slots');
console.log('PASS: configurable four-request admission stays bounded and releases all queued work on abort');

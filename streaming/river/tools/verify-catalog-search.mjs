import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const source = fs.readFileSync(new URL('../web/viewer.js', import.meta.url), 'utf8');
const refresh = source.slice(source.indexOf('async function refreshAvailableAssets('), source.indexOf("catalogNext.addEventListener('click'"));
const search = source.slice(source.indexOf('function searchCatalog()'), source.indexOf("catalogSearch.addEventListener('input', searchCatalog)"));
const requests = [];
const context = vm.createContext({
  URLSearchParams, AbortController, apiBase: 'http://fixture',
  catalogDraft: false, catalogScanned: null, catalogGeneration: 0, catalogAbort: null,
  catalogPageHistory: [], catalogPageStart: null, catalogLoading: false, catalogError: '',
  catalogSource: { value: 'all' }, catalogSearch: { value: 'first' }, catalogSearchMode: { value: 'prefix' },
  catalogPageSize: { value: '100' }, catalogNext: { disabled: false }, catalogPrevious: { disabled: true },
  catalogCursor: null, availableAssets: [], renderCatalog() {},
  fetch(url, options) { return new Promise(resolve => requests.push({ url, options, resolve })); },
});
vm.runInContext(refresh + '\n' + search, context);

const reply = (i, objects, nextCursor) => requests[i].resolve({
  ok: true, json: async () => ({ objects, nextCursor, scannedObjects: 100 }),
});
context.searchCatalog();
context.catalogSearch.value = 'folder/image';
context.searchCatalog();
assert.equal(requests.length, 0, 'Typing must never list S3');
assert.equal(context.catalogNext.disabled, true);

const first = context.refreshAvailableAssets();
const params = new URL(requests[0].url).searchParams;
assert.equal(params.get('source'), 'all');
assert.equal(params.get('keyPrefix'), 'folder/image');
assert.equal(params.get('limit'), '100');
reply(0, [], 'second-page');
await first;
assert.equal(requests.length, 1, 'An empty page must not auto-advance');
assert.equal(context.catalogNext.disabled, false);

const next = context.refreshAvailableAssets({ next: true });
assert.equal(new URL(requests[1].url).searchParams.get('cursor'), 'second-page');
reply(1, [{ key: 'folder/image.jp2' }], 'third-page');
await next;
assert.equal(context.availableAssets.length, 1);
assert.equal(context.catalogPrevious.disabled, false);

const previous = context.refreshAvailableAssets({ previous: true });
assert.equal(new URL(requests[2].url).searchParams.has('cursor'), false);
reply(2, [], 'second-page');
await previous;
assert.equal(context.catalogPrevious.disabled, true);

const stale = context.refreshAvailableAssets({ next: true });
context.catalogSearchMode.value = 'contains';
context.catalogSearch.value = 'needle';
context.catalogPageSize.value = '25';
context.searchCatalog();
assert.equal(requests[3].options.signal.aborted, true);
assert.equal(requests.length, 4);
const changed = context.refreshAvailableAssets();
const changedParams = new URL(requests[4].url).searchParams;
assert.equal(changedParams.get('q'), 'needle');
assert.equal(changedParams.get('limit'), '25');
assert.equal(changedParams.has('cursor'), false);
reply(4, [{ key: 'changed.jp2' }], null);
await changed;
reply(3, [{ key: 'stale.jp2' }], 'stale');
await stale;
assert.equal(context.availableAssets[0].key, 'changed.jp2');
assert.equal(context.catalogCursor, null);
console.log('PASS: zero listings while typing, one request per action, prefix and contains, page sizing and stale cancellation');

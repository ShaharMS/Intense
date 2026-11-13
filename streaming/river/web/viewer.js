const stage = document.querySelector('#stage');
const canvas = document.querySelector('#viewport');
const boundaryCanvas = document.querySelector('#tile-boundaries');
const boundaryColor = document.querySelector('#boundary-color');
const boundaryEnabled = document.querySelector('#boundary-enabled');
const boundaryWidth = document.querySelector('#boundary-width');
const catalogSearch = document.querySelector('#catalog-search');
const catalogSearchMode = document.querySelector('#catalog-search-mode');
let catalogDraft = false;
let catalogScanned = null;
let catalogInitialized = false;
const catalogPageSize = document.querySelector('#catalog-page-size');
const catalogSort = document.querySelector('#catalog-sort');
const catalogDirection = document.querySelector('#catalog-direction');
const catalogList = document.querySelector('#catalog-list');
const catalogCount = document.querySelector('#catalog-count');
const catalogSource = document.querySelector('#catalog-source');
const catalogNext = document.querySelector('#catalog-next');
const catalogPrevious = document.querySelector('#catalog-previous');
let catalogPageHistory = [];
let catalogPageStart = null;
let catalogCursor = null;
let catalogGeneration = 0;
let catalogAbort = null;
let catalogLoading = false;
let catalogError = '';
const status = document.querySelector('#status');
let trace = document.querySelector('#trace');
const latencyTrace = document.querySelector('#latency-trace');
const latencyTotal = document.querySelector('#latency-total');
const requestButton = document.querySelector('#request');
const fitButton = document.querySelector('#fit');
const randomButton = document.querySelector('#random');
const decoderDriver = document.querySelector('#decoder-driver');
const deliveryMethod = document.querySelector('#delivery-method');
const rasterFormat = document.querySelector('#raster-format');
const boundaryContext = boundaryCanvas.getContext('2d');
const inputs = Object.fromEntries(['asset', 'x', 'y', 'width', 'height', 'zoom'].map((id) => [id, document.querySelector(`#${id}`)]));
let savedApiOrigin;
try { savedApiOrigin = localStorage.getItem('river.apiUrl'); } catch {}
const apiOrigin = new URL(savedApiOrigin || window.RIVER_API_ORIGIN || location.origin);
const apiBase = apiOrigin.toString().replace(/\/$/, '');
const wmtsConcurrency = Math.max(1, Math.min(6, Math.floor(Number(
  new URL(location.href).searchParams.get('wmtsConcurrency') ?? window.RIVER_WMTS_CONCURRENCY
) || 2)));
document.querySelector('#api-origin').textContent = apiBase;
document.querySelector('#api-url').value = apiBase;
function switchApi(value) {
  try {
    const url = new URL(value);
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error('Use an HTTP or HTTPS API URL without credentials, query, or fragment.');
    localStorage.setItem('river.apiUrl', url.toString().replace(/\/$/, ''));
    location.reload();
  } catch (error) { status.textContent = `Cannot switch API: ${error.message}`; }
}
document.querySelector('#api-url-form').addEventListener('submit', event => { event.preventDefault(); switchApi(document.querySelector('#api-url').value.trim()); });
document.querySelector('#api-url-reset').addEventListener('click', () => switchApi(location.origin));

let assetManifest = null;
let availableAssets = [];
let activeViewport = null;
let session = null;
let latestSequence = 0;
let acceptedTrace = null;
let tileCount = 0;
let drag = null;
let observedStageSize = '';
let requestStartedAt = new Map();
let wmtsGeneration = 0;
let wmtsAbort = null;
let wmtsTiles = null;
let activeLatencyRows = null;
const traceGroups = new Map();
let activeRenderMode = null;
let assetLoadGeneration = 0;
let assetLoadAbort = null;
let catalogDescending = false;
const renderer = createRenderer(canvas);
// Keep this in lockstep with river_core::TILE_SIZE. It is intentionally a
// display-space constant: tile coordinates are reference-grid coordinates.
const RIVER_TILE_SIZE = 512;

function clamp(value, minimum, maximum) {
  return Math.min(Math.max(value, minimum), maximum);
}

function stageSize() {
  const rect = stage.getBoundingClientRect();
  return { width: Math.max(1, Math.round(rect.width)), height: Math.max(1, Math.round(rect.height)) };
}

function maxZoom() {
  const { width, height } = stageSize();
  const fit = Math.max(assetManifest.width / width, assetManifest.height / height);
  return Math.min(assetManifest.resolutionLevels - 1, Math.max(0, Math.ceil(Math.log2(fit))));
}

function viewportAt(centerX, centerY, requestedZoom) {
  const { width: displayWidth, height: displayHeight } = stageSize();
  const zoom = clamp(Math.round(requestedZoom), 0, maxZoom());
  const scale = 2 ** zoom;
  const width = Math.min(assetManifest.width, Math.max(1, Math.round(displayWidth * scale)));
  const height = Math.min(assetManifest.height, Math.max(1, Math.round(displayHeight * scale)));
  return {
    assetId: assetManifest.assetId,
    // Moving inside the same tile set must keep accepting in-flight frames.
    sequence: latestSequence,
    x: Math.round(clamp(centerX - width / 2, 0, assetManifest.width - width)),
    y: Math.round(clamp(centerY - height / 2, 0, assetManifest.height - height)),
    width,
    height,
    zoom,
  };
}

function viewportAtAnchor(anchorX, anchorY, fractionX, fractionY, zoom) {
  const viewport = viewportAt(anchorX, anchorY, zoom);
  viewport.x = Math.round(clamp(anchorX - viewport.width * fractionX, 0, assetManifest.width - viewport.width));
  viewport.y = Math.round(clamp(anchorY - viewport.height * fractionY, 0, assetManifest.height - viewport.height));
  return viewport;
}

function fitViewport() {
  return viewportAt(assetManifest.width / 2, assetManifest.height / 2, maxZoom());
}

function randomViewport() {
  return viewportAt(
    Math.random() * assetManifest.width,
    Math.random() * assetManifest.height,
    Math.floor(Math.random() * (maxZoom() + 1)),
  );
}

function layoutFor(viewport) {
  const size = stageSize();
  const scale = 2 ** viewport.zoom;
  const width = viewport.width / scale;
  const height = viewport.height / scale;
  return {
    stageWidth: size.width,
    stageHeight: size.height,
    scale,
    width,
    height,
    offsetX: Math.max(0, (size.width - width) / 2),
    offsetY: Math.max(0, (size.height - height) / 2),
  };
}

function syncControls() {
  inputs.x.value = activeViewport.x;
  inputs.y.value = activeViewport.y;
  inputs.width.value = activeViewport.width;
  inputs.height.value = activeViewport.height;
  inputs.zoom.value = activeViewport.zoom;
  inputs.zoom.max = maxZoom();
}

function drawTileBoundaries(viewport, layout = layoutFor(viewport)) {
  const width = layout.stageWidth;
  const height = layout.stageHeight;
  if (boundaryCanvas.width !== width) boundaryCanvas.width = width;
  if (boundaryCanvas.height !== height) boundaryCanvas.height = height;
  boundaryContext.clearRect(0, 0, width, height);
  if (isLimited() || !boundaryEnabled.checked) return;
  const scale = layout.scale;
  const sourceTile = RIVER_TILE_SIZE * scale;
  const left = layout.offsetX;
  const top = layout.offsetY;
  const right = left + layout.width;
  const bottom = top + layout.height;
  boundaryContext.save();
  boundaryContext.beginPath();
  boundaryContext.rect(left, top, layout.width, layout.height);
  boundaryContext.clip();
  const thickness = Number(boundaryWidth.value);
  const inset = thickness / 2 + 1;
  boundaryContext.lineJoin = 'miter';
  boundaryContext.beginPath();
  const firstColumn = Math.floor(viewport.x / sourceTile);
  const lastColumn = Math.ceil((viewport.x + viewport.width) / sourceTile);
  const firstRow = Math.floor(viewport.y / sourceTile);
  const lastRow = Math.ceil((viewport.y + viewport.height) / sourceTile);
  for (let row = firstRow; row < lastRow; row += 1) {
    for (let column = firstColumn; column < lastColumn; column += 1) {
      const x0 = Math.max(left + inset, left + (column * sourceTile - viewport.x) / scale);
      const y0 = Math.max(top + inset, top + (row * sourceTile - viewport.y) / scale);
      const x1 = Math.min(right - inset, left + (Math.min((column + 1) * sourceTile, assetManifest.width) - viewport.x) / scale);
      const y1 = Math.min(bottom - inset, top + (Math.min((row + 1) * sourceTile, assetManifest.height) - viewport.y) / scale);
      if (x1 > x0 && y1 > y0) boundaryContext.rect(x0, y0, x1 - x0, y1 - y0);
    }
  }
  // Shared edges use identical screen coordinates. Draw all dark halos first
  // so neighboring tile borders cannot obscure one another's colored edges.
  boundaryContext.strokeStyle = '#101214';
  boundaryContext.lineWidth = thickness + 2;
  boundaryContext.stroke();
  boundaryContext.strokeStyle = boundaryColor.value;
  boundaryContext.lineWidth = thickness;
  boundaryContext.stroke();
  boundaryContext.restore();
}

function presentViewport() {
  if (!assetManifest || !activeViewport) return;
  const layout = layoutFor(activeViewport);
  canvas.classList.toggle('is-rendering', !isLimited());
  renderer.resize(layout.stageWidth, layout.stageHeight);
  drawTileBoundaries(activeViewport, layout);
  observedStageSize = `${layout.stageWidth}x${layout.stageHeight}`;
  syncControls();
}

function isLimited() {
  if (typeof assetManifest?.tilesAvailable === 'boolean') return !assetManifest.tilesAvailable;
  return assetManifest?.capability?.status === 'limited';
}

function ms(value) {
  return `${Math.max(0, value).toFixed(1)} ms`;
}

function resetLatencyTrace(sequence) {
  if (latencyTrace.children.length) latencyTrace.append(document.createElement('hr'));
  const group = document.createElement('section');
  group.className = 'trace-request';
  const heading = document.createElement('h3');
  heading.textContent = `Request ${sequence} · ${deliveryMethod.selectedOptions[0].textContent}`;
  activeLatencyRows = document.createElement('div');
  activeLatencyRows.className = 'request-latencies';
  trace.removeAttribute('id');
  const detail = document.createElement('pre');
  detail.id = 'trace';
  detail.className = 'trace-detail';
  if (!traceGroups.size) trace.remove();
  trace = detail;
  group.append(heading, activeLatencyRows, detail);
  latencyTrace.append(group);
  traceGroups.set(sequence, { rows: activeLatencyRows, detail });
  latencyTotal.textContent = `sequence ${sequence}`;
  scrollTraces();
}

let traceScrollFrame = null;
function scrollTraces() {
  if (traceScrollFrame !== null) return;
  traceScrollFrame = requestAnimationFrame(() => {
    traceScrollFrame = null;
    const section = document.querySelector('#traces-section');
    section.scrollTop = section.scrollHeight;
  });
}
function setTraceDetail(value, sequence = activeViewport?.sequence) {
  const detail = traceGroups.get(sequence)?.detail ?? trace;
  detail.textContent = value;
  scrollTraces();
}

function renderLatencyTrace(sample) {
  const maxTotal = Math.max(sample.totalMs, 1);
  const entry = sample;
    const row = document.createElement('div');
    row.className = 'latency-row';
    row.dataset.duringDrag = String(!!drag);
    const label = document.createElement('span');
    label.textContent = `${entry.pass} ${entry.tile}`;
    const bar = document.createElement('div');
    bar.className = 'latency-bar';
    bar.title = entry.detail;
    for (const [kind, value] of entry.segments) {
      if (value <= 0) continue;
      const segment = document.createElement('span');
      segment.className = `latency-segment latency-${kind}`;
      segment.style.width = `${Math.max(1, value / maxTotal * 100)}%`;
      bar.append(segment);
    }
    const total = document.createElement('span');
    total.textContent = ms(entry.totalMs);
    row.append(label, bar, total);
    (traceGroups.get(sample.sequence ?? activeViewport?.sequence)?.rows ?? activeLatencyRows ?? latencyTrace).append(row);
  latencyTotal.textContent = `presented ${ms(sample.totalMs)}`;
  scrollTraces();
}

async function loadAsset({ preserve = false, randomize = false } = {}) {
  wmtsAbort?.abort();
  wmtsGeneration += 1;
  assetLoadAbort?.abort();
  const generation = ++assetLoadGeneration;
  const assetId = inputs.asset.value.trim();
  if (!assetId) return;
  if (activeViewport?.assetId !== assetId) {
    ++latestSequence;
    assetManifest = null; activeViewport = null;
    renderer.clear();
    boundaryContext.clearRect(0, 0, boundaryCanvas.width, boundaryCanvas.height);
  }
  const controller = new AbortController();
  assetLoadAbort = controller;
  status.textContent = 'checking asset manifest…';
  try {
    const response = await fetch(`${apiBase}/api/assets/${encodeURIComponent(assetId)}/manifest`, { signal: controller.signal });
    if (!response.ok) {
      const detail = await response.json().catch(() => ({}));
      throw new Error(detail.message || detail.code || `manifest request returned HTTP ${response.status}`);
    }
    const manifest = await response.json();
    if (generation !== assetLoadGeneration || inputs.asset.value.trim() !== assetId) return;
    inputs.asset.value = manifest.assetId;
    assetManifest = manifest;
    applyAssetDecoderCapabilities();
    const centerX = preserve && activeViewport?.assetId === assetId ? activeViewport.x + activeViewport.width / 2 : assetManifest.width / 2;
    const centerY = preserve && activeViewport?.assetId === assetId ? activeViewport.y + activeViewport.height / 2 : assetManifest.height / 2;
    const zoom = preserve && activeViewport?.assetId === assetId ? activeViewport.zoom : maxZoom();
    activeViewport = randomize ? randomViewport() : viewportAt(centerX, centerY, zoom);
    renderCatalog();
    presentViewport();
    requestViewport();
  } catch (error) {
    if (generation !== assetLoadGeneration || error.name === 'AbortError') return;
    status.textContent = `River unavailable: ${error.message}`;
  } finally {
    if (assetLoadAbort === controller) assetLoadAbort = null;
  }
}

async function refreshAvailableAssets({ next = false, previous = false } = {}) {
  if (catalogDraft && (next || previous)) return;
  catalogDraft = false;
  const generation = ++catalogGeneration;
  catalogAbort?.abort();
  catalogAbort = new AbortController();
  catalogNext.disabled = true;
  catalogPrevious.disabled = true;
  catalogLoading = true;
  catalogError = '';
  availableAssets = [];
  renderCatalog();
  const start = previous ? catalogPageHistory.at(-1) : next ? catalogCursor : null;
  if (!next && !previous) { catalogPageHistory = []; catalogPageStart = null; }
  try {
    const params = new URLSearchParams({ limit: catalogPageSize.value });
    if (catalogSource.value) params.set('source', catalogSource.value);
    if (catalogSource.value && catalogSearch.value.trim()) params.set(catalogSearchMode.value === 'prefix' ? 'keyPrefix' : 'q', catalogSearch.value.trim());
    if (start) params.set('cursor', start);
    const response = await fetch(`${apiBase}/api/assets?${params}`, { signal: catalogAbort.signal });
    if (!response.ok) throw new Error(await responseError(response));
    const body = await response.json();
    if (generation !== catalogGeneration) return;
    const following = Array.isArray(body) ? response.headers.get('x-river-next-cursor') : body.nextCursor;
    if (following && following === start) throw new Error('Server returned a non-advancing search cursor');
    if (next) catalogPageHistory.push(catalogPageStart);
    if (previous) catalogPageHistory.pop();
    catalogPageStart = start;
    availableAssets = Array.isArray(body) ? body : body.objects;
    catalogScanned = body.scannedObjects ?? null;
    catalogCursor = following;
    catalogNext.disabled = !catalogCursor;
    catalogPrevious.disabled = !catalogPageHistory.length;
    catalogLoading = false;
    renderCatalog();
  } catch (error) {
    if (generation !== catalogGeneration || error.name === 'AbortError') return;
    catalogLoading = false;
    catalogError = `Catalog unavailable: ${error.message}`;
    catalogNext.disabled = !catalogCursor;
    catalogPrevious.disabled = !catalogPageHistory.length;
    renderCatalog();
  }
}
catalogNext.addEventListener('click', () => { void refreshAvailableAssets({ next: true }); });
catalogPrevious.addEventListener('click', () => { void refreshAvailableAssets({ previous: true }); });
catalogSource.addEventListener('change', () => { catalogCursor = null; catalogSearchMode.disabled = !catalogSource.value; void refreshAvailableAssets(); });

function assetName(asset) {
  return asset.name || (asset.assetId === '94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290' ? 'Bretagne2.j2k' : asset.assetId);
}
function assetBytes(asset) { return asset.size ?? asset.quality?.cumulativeLayerBytes?.at(-1) ?? 0; }
function renderCatalog() {
  const query = catalogSearch.value.trim().toLocaleLowerCase();
  const items = catalogSource.value ? [...availableAssets] : availableAssets.filter(asset => `${assetName(asset)} ${asset.assetId}`.toLocaleLowerCase().includes(query));
  const metric = asset => catalogSort.value === 'pixels' ? asset.width * asset.height : catalogSort.value === 'bytes' ? assetBytes(asset) : asset[catalogSort.value];
  items.sort((a, b) => {
    const order = catalogSort.value === 'name' ? assetName(a).localeCompare(assetName(b), undefined, { numeric: true }) : metric(a) - metric(b);
    return (catalogDescending ? -order : order) || assetName(a).localeCompare(assetName(b), undefined, { numeric: true }) || a.assetId.localeCompare(b.assetId);
  });
  catalogCount.textContent = catalogDraft ? 'Search to apply these settings.' : catalogError || (catalogLoading ? 'Searching…' :
    `${items.length} images on this page${catalogScanned == null ? '' : ` · ${catalogScanned} keys checked`}${catalogCursor ? ' · More pages available' : ''}`);
  catalogList.replaceChildren(...items.map(asset => {
    const button = document.createElement('button');
    button.className = 'catalog-item';
    button.setAttribute('aria-current', String(asset.assetId === activeViewport?.assetId));
    button.title = `${assetName(asset)}\n${asset.assetId}`;
    const name = document.createElement('span'); name.className = 'catalog-name'; name.textContent = assetName(asset);
    const metrics = document.createElement('span'); metrics.className = 'catalog-metrics';
    metrics.textContent = asset.width == null ? `${(assetBytes(asset) / 1048576).toFixed(1)} MiB · metadata detected on selection` : `${asset.width.toLocaleString()} × ${asset.height.toLocaleString()} · ${(asset.width * asset.height / 1e6).toFixed(1)} MP\n${(assetBytes(asset) / 1048576).toFixed(1)} MiB · ${asset.bitDepth}-bit · ${asset.resolutionLevels} levels`;
    metrics.style.whiteSpace = 'pre-line';
    button.append(name, metrics);
    button.addEventListener('click', () => {
      if (asset.assetId === activeViewport?.assetId && inputs.asset.value === asset.assetId) return;
      inputs.asset.value = asset.assetId;
      ++latestSequence; ++wmtsGeneration;
      void loadAsset();
    });
    return button;
  }));
  if (!items.length) { const note = document.createElement('p'); note.className = 'note'; note.textContent = catalogError || (catalogLoading ? 'Searching images…' : catalogCursor ? 'No matching images on this page. Continue to the next page.' : query ? 'No matching images on this page.' : 'No images available.'); catalogList.append(note); }
}

async function refreshViewerOptions() {
  const [driverResponse, formatResponse, wsCapabilities] = await Promise.all([
    fetch(`${apiBase}/api/drivers`),
    fetch(`${apiBase}/api/formats`),
    fetch(`${apiBase}/api/ws-capabilities`)
      .then(response => response.ok ? response.json() : null)
      .catch(() => null),
  ]);
  if (!driverResponse.ok || !formatResponse.ok) throw new Error('decoder capabilities are unavailable');
  const drivers = await driverResponse.json();
  const formats = await formatResponse.json();
  for (const info of drivers) {
    if (![...decoderDriver.options].some(option => option.value === info.id)) {
      decoderDriver.add(new Option(info.name || info.id, info.id));
    }
  }
  for (const option of decoderDriver.options) {
    if (option.value === 'auto') {
      option.dataset.serviceAvailable = String(wsCapabilities?.autoDriver === true);
      continue;
    }
    const info = drivers.find((driver) => driver.id === option.value);
    option.disabled = !info?.available;
    option.dataset.serviceAvailable = String(Boolean(info?.available));
    option.title = [info?.reason, info?.progressiveQuality === false ? 'Streams spatial tiles at full quality without JPEG 2000 quality-layer previews.' : null].filter(Boolean).join(' ');
  }
  for (const option of rasterFormat.options) {
    const info = formats.find((format) => format.id === option.value);
    option.disabled = !info?.available;
    option.title = info?.reason ?? '';
  }
  applyAssetDecoderCapabilities();
  if (rasterFormat.selectedOptions[0]?.disabled) rasterFormat.value = 'webp';
}

function applyAssetDecoderCapabilities() {
  const supported = assetManifest?.supportedDecoderDrivers;
  for (const option of decoderDriver.options) {
    const available = option.dataset.serviceAvailable !== 'false';
    option.disabled = option.value === 'auto'
      ? deliveryMethod.value !== 'wmts' && !available
      : !available || (Array.isArray(supported) && !supported.includes(option.value));
  }
  if (decoderDriver.selectedOptions[0]?.disabled) {
    const usable = [...decoderDriver.options].find(option => !option.disabled);
    if (!usable) throw new Error('This image needs an available HTJ2K decoder (JP2Grok or nvJPEG2000).');
    decoderDriver.value = usable.value;
  }
}

function requestViewport({ eager = false } = {}) {
  if (!assetManifest || !activeViewport) return;
  if (isLimited()) {
    status.textContent = `Image unavailable: ${(assetManifest.capability?.reasons ?? ['No decodable source is available.']).join('; ')}`;
    setTraceDetail([
      `asset: ${activeViewport.assetId}`,
      `reference grid: ${assetManifest.width} × ${assetManifest.height}`,
      `viewport: ${activeViewport.x},${activeViewport.y} ${activeViewport.width} × ${activeViewport.height}`,
      `zoom: 1:${2 ** activeViewport.zoom} source pixels per screen pixel`,
      ...(assetManifest.capability?.reasons ?? []).map((reason) => `limited: ${reason}`),
    ].join('\n'));
    return;
  }
  const transport = deliveryMethod.value;
  if (transport !== 'wmts') { wmtsAbort?.abort(); wmtsGeneration += 1; }
  const format = rasterFormat.value;
  const driver = decoderDriver.value;
  const renderMode = `${activeViewport.assetId}-${transport}-${format}-${driver}`;
  const modeChanged = activeRenderMode !== renderMode;
  activeRenderMode = renderMode;
  if (transport === 'wmts') {
    const viewport = { ...activeViewport, sequence: ++latestSequence };
    activeViewport = viewport;
    renderer.begin(viewport, layoutFor(viewport), modeChanged);
    resetLatencyTrace(viewport.sequence);
    tileCount = 0;
    void requestWmtsViewport(viewport, format, driver);
    return;
  }
  if (!session || session.readyState !== WebSocket.OPEN) return;
  const visible = [];
  if (eager) {
    const edge = RIVER_TILE_SIZE * 2 ** activeViewport.zoom;
    for (let y = Math.floor(activeViewport.y / edge); y <= Math.floor((activeViewport.y + activeViewport.height - 1) / edge); y += 1) {
      for (let x = Math.floor(activeViewport.x / edge); x <= Math.floor((activeViewport.x + activeViewport.width - 1) / edge); x += 1) {
        if (!renderer.hasTile(y, x, activeViewport.zoom, false)) visible.push({ x, y, z: activeViewport.zoom });
      }
    }
    if (!visible.length) return;
    visible.sort((a, b) => (a.x + .5 - (activeViewport.x + activeViewport.width / 2) / edge) ** 2 + (a.y + .5 - (activeViewport.y + activeViewport.height / 2) / edge) ** 2 - ((b.x + .5 - (activeViewport.x + activeViewport.width / 2) / edge) ** 2 + (b.y + .5 - (activeViewport.y + activeViewport.height / 2) / edge) ** 2));
  }
  const viewport = { ...activeViewport, sequence: ++latestSequence };
  activeViewport = viewport;
  requestStartedAt.set(viewport.sequence, performance.now());
  while (requestStartedAt.size > 8) requestStartedAt.delete(requestStartedAt.keys().next().value);
  resetLatencyTrace(viewport.sequence);
  tileCount = 0;
  renderer.begin(viewport, layoutFor(viewport), modeChanged);
  session.send(JSON.stringify(eager
    ? { type: 'tiles', batch: { assetId: viewport.assetId, sequence: viewport.sequence, tiles: visible }, format, driver }
    : { type: 'viewport', viewport, format, driver, progressivePatches: true }));
  status.textContent = `decoding JPEG 2000 tiles with ${decoderDriver.selectedOptions[0].textContent} (${format.toUpperCase()})…`;
  setTraceDetail(`sequence ${viewport.sequence} requested\nwaiting for ${decoderDriver.selectedOptions[0].textContent} tiles…`);
}

async function requestWmtsViewport(viewport, format, driver) {
  const mode = `${viewport.assetId}:${viewport.zoom}:${format}:${driver}`;
  if (!wmtsTiles || wmtsTiles.mode !== mode || wmtsTiles.controller.signal.aborted) {
    wmtsAbort?.abort();
    const controller = new AbortController();
    wmtsAbort = controller;
    wmtsTiles = { mode, controller, generation: ++wmtsGeneration, requests: new Map(), presenting: new Map(), queue: [], active: 0, visible: new Set() };
  }
  const tiles = wmtsTiles;
  const controller = tiles.controller;
  const generation = tiles.generation;
  const requestAt = performance.now();
  const driverLabel = decoderDriver.selectedOptions[0].textContent;
  const current = () => !controller.signal.aborted && generation === wmtsGeneration;
  const jobKey = job => `${job.preview ? 'overview:' : ''}${job.reduction}:${job.col}:${job.row}`;
  status.textContent = 'requesting WMTS tiles…';
  setTraceDetail(`WMTS ${format.toUpperCase()} via ${driverLabel} sequence ${viewport.sequence}\nrequesting GetTile images…`, viewport.sequence);
  const maxLevel = assetManifest.resolutionLevels - 1;
  const jobs = [];
  const measurements = [];
  let firstVisible = null;
  let firstDetail = null;
  const addJobs = (reduction, preview) => {
    const edge = RIVER_TILE_SIZE * 2 ** reduction;
    const group = [];
    for (let row = Math.floor(viewport.y / edge); row <= Math.floor((viewport.y + viewport.height - 1) / edge); row += 1) {
      for (let col = Math.floor(viewport.x / edge); col <= Math.floor((viewport.x + viewport.width - 1) / edge); col += 1) {
        const distance = (col + 0.5 - (viewport.x + viewport.width / 2) / edge) ** 2 + (row + 0.5 - (viewport.y + viewport.height / 2) / edge) ** 2;
        group.push({ row, col, reduction, preview, distance });
      }
    }
    jobs.push(...group.sort((a, b) => a.distance - b.distance));
  };
  // A native coarsest-resolution image is visible promptly while detail
  // arrives. This is an explicit requested preview, not a generated pyramid.
  if (viewport.zoom < maxLevel) addJobs(maxLevel, true);
  addJobs(viewport.zoom, false);
  const detailCount = jobs.filter(job => !job.preview).length;
  tiles.visible = new Set(jobs.map(jobKey));
  for (const [key, pending] of tiles.requests) {
    if (!tiles.visible.has(key)) {
      pending.controller.abort();
      tiles.requests.delete(key);
    }
  }
  const localPresented = new Set();
  const run = async (job) => {
    const { row, col, reduction, preview } = job;
    const key = jobKey(job);
    if (!current() || !tiles.visible.has(key)) return;
    if (renderer.hasTile(row, col, reduction, preview)) {
      if (!preview) localPresented.add(key);
      return;
    }
    const owner = tiles.presenting.get(key);
    if (owner) {
      await owner;
      if (renderer.hasTile(row, col, reduction, preview)) {
        if (!preview) localPresented.add(key);
      } else if (current() && tiles.visible.has(key)) return run(job);
      return;
    }
    let finishPresentation;
    tiles.presenting.set(key, new Promise(resolve => { finishPresentation = resolve; }));
    try {
    const matrix = maxLevel - reduction;
    const url = new URL(`${apiBase}/wmts`);
    for (const [key, value] of Object.entries({ SERVICE: 'WMTS', REQUEST: 'GetTile', VERSION: '1.0.0', LAYER: viewport.assetId, STYLE: 'default', FORMAT: `image/${format}`, TILEMATRIXSET: `RiverPixel-${viewport.assetId}`, TILEMATRIX: matrix, TILEROW: row, TILECOL: col })) url.searchParams.set(key, value);
    if (driver !== 'auto') url.searchParams.set('DRIVER', driver);
    if (preview && driver !== 'nvjpeg2000') url.searchParams.set('LAYERS', '1');
    const started = performance.now();
    const clientQueue = started - requestAt;
    const { response, blob } = await visibleWmtsTile(tiles, key, url);
    const receivedAt = performance.now();
    if (!current() || !tiles.visible.has(key)) return;
    const decodeStarted = performance.now();
    const bitmap = await createImageBitmap(blob);
    const browserDecode = performance.now() - decodeStarted;
    if (!current() || !tiles.visible.has(key)) { bitmap.close(); return; }
    const sourceTileEdge = RIVER_TILE_SIZE * 2 ** reduction;
    const scale = 2 ** viewport.zoom;
    const header = { tileX: col, tileY: row, sourceTileEdge, preview,
      outputWidth: Math.ceil(Math.min(sourceTileEdge, assetManifest.width - col * sourceTileEdge) / scale),
      outputHeight: Math.ceil(Math.min(sourceTileEdge, assetManifest.height - row * sourceTileEdge) / scale), sourceLayers: preview ? 0 : 1 };
    const drawing = renderer.draw(header, bitmap, activeViewport, layoutFor(activeViewport));
    bitmap.close();
    const submittedAt = performance.now();
    const presentedAt = await drawing.presented;
    if (!current() || !tiles.visible.has(key)) return;
    firstVisible ??= presentedAt - requestAt;
    if (!preview) firstDetail ??= presentedAt - requestAt;
    const timing = Object.fromEntries(['queue', 'decode', 'encode', 'server', 'source-cache'].map((name) => [name, Number(response.headers.get(`x-river-${name}-ms`)) || 0]));
    const cacheStatus = response.headers.get('x-river-source-cache') || 'unknown';
    const cacheMs = timing['source-cache'];
    const sourceIo = `S3 source: ${(Number(response.headers.get('x-river-source-read-bytes')) / 1024).toFixed(1)} KiB fetched; ${Number(response.headers.get('x-river-source-range-requests'))} ranges; ${(Number(response.headers.get('x-river-source-cache-hit-bytes')) / 1024).toFixed(1)} KiB reused`;
    const serverOther = Math.max(0, timing.server - timing.decode - timing.encode - cacheMs);
    const transport = Math.max(0, receivedAt - started - timing.server - timing.queue);
    const paintWait = Math.max(0, presentedAt - submittedAt);
    const total = presentedAt - requestAt;
    const detail = `WMTS ${preview ? 'native overview' : 'detail'} ${format.toUpperCase()} GetTile ${col},${row} via ${driverLabel}; trace ${response.headers.get('x-river-trace-id')}; ${response.headers.get('x-river-decoder-route')}; client queue ${ms(clientQueue)}; server queue ${ms(timing.queue)}; J2K ${ms(timing.decode)}; encode ${ms(timing.encode)}; server ${ms(timing.server)}; transport/dispatch estimate ${ms(transport)}; browser decode ${ms(browserDecode)}; WebGL upload ${ms(drawing.uploadMs)}; frame wait ${ms(paintWait)}; viewport ${ms(total)}`;
    renderLatencyTrace({ sequence: viewport.sequence, pass: preview ? 'overview' : 'tile', tile: `${col},${row}`, totalMs: total, detail: `${detail}; source cache ${cacheStatus} (${ms(cacheMs)} foreground)`, segments: [['client', clientQueue], ['queue', timing.queue], ['cache', cacheMs], ['decode', timing.decode], ['encode', timing.encode], ['server', serverOther], ['wire', transport], ['browser', browserDecode], ['gl', drawing.uploadMs], ['paint', paintWait]] });
    if (!preview) {
      localPresented.add(key);
      tileCount = localPresented.size;
      measurements.push({ server: timing.server, transport, clientQueue, browserDecode, upload: drawing.uploadMs, paintWait, service: presentedAt - started, viewport: total });
    }
    const statistic = (name, percentile = 0.5) => {
      const values = measurements.map(m => m[name]).sort((a, b) => a - b);
      return values.length ? (percentile === 0.5 ? (values[Math.floor((values.length - 1) / 2)] + values[Math.floor(values.length / 2)]) / 2 : values[Math.ceil(values.length * percentile) - 1]) : 0;
    };
    latencyTotal.textContent = `first ${ms(firstVisible)} · detail ${tileCount}/${detailCount}`;
    setTraceDetail(`driver: ${driverLabel}\nmethod: WMTS GetTile; format: ${format.toUpperCase()}\nsequence: ${viewport.sequence}; detail tiles presented: ${tileCount}/${detailCount}\nlast ${preview ? 'overview' : 'detail'}: ${col},${row}; matrix ${matrix}\ntrace: ${response.headers.get('x-river-trace-id')}\nserver: ${response.headers.get('x-river-decoder-route')}; queue ${ms(timing.queue)}; J2K ${ms(timing.decode)}; encode ${ms(timing.encode)}; total ${ms(timing.server)}\nclient: queued ${ms(clientQueue)}; transport/dispatch estimate ${ms(transport)}; browser decode ${ms(browserDecode)}; upload ${ms(drawing.uploadMs)}; frame wait ${ms(paintWait)}\nviewport: first visible ${ms(firstVisible)}; first detail ${firstDetail == null ? 'pending' : ms(firstDetail)}; latest frame ${ms(total)}\n${measurements.length} detail samples: backend median ${ms(statistic('server'))}, p95 ${ms(statistic('server', 0.95))}; service-to-frame median ${ms(statistic('service'))}, p95 ${ms(statistic('service', 0.95))}; transport/dispatch median ${ms(statistic('transport'))}; browser decode median ${ms(statistic('browserDecode'))}\nsource cache: ${cacheStatus}; foreground ${ms(cacheMs)}\n${sourceIo}`, viewport.sequence);
    if (activeViewport.sequence === viewport.sequence) status.textContent = tileCount === detailCount ? `WMTS: ${tileCount} tiles loaded` : `WMTS: image visible; refining ${tileCount}/${detailCount} tiles`;
    } finally {
      tiles.presenting.delete(key);
      finishPresentation();
    }
  };
  let next = 0;
  const failures = [];
  // HTTP/1 browsers admit six connections per origin. Keep the browser's
  // hidden socket queue short, prioritize the center, and cancel old work.
  try {
    await Promise.all(Array.from({ length: Math.min(wmtsConcurrency, jobs.length) }, async () => {
      while (next < jobs.length && current()) {
        try { await run(jobs[next++]); }
        catch (error) { if (error.name !== "AbortError") failures.push(error.message); }
      }
    }));
    if (current() && activeViewport.sequence === viewport.sequence) {
      const ready = jobs.filter(job => !job.preview && renderer.hasTile(job.row, job.col, job.reduction, job.preview)).length;
      tileCount = ready;
      status.textContent = failures.length ? `WMTS: ${ready}/${detailCount} tiles loaded; ${failures.length} failed (${failures[0]}). Retry the view to reload missing tiles.` : ready === detailCount ? `WMTS: ${ready} tiles loaded` : `WMTS: image visible; refining ${ready}/${detailCount} tiles`;
      if (!measurements.length && ready === detailCount) {
        latencyTotal.textContent = `detail ${ready}/${detailCount} · visible textures reused`;
        setTraceDetail(`sequence: ${viewport.sequence}\n${ready}/${detailCount} detail tiles already visible; reused textures.`, viewport.sequence);
      }
    }
  } catch (error) {
    if (current() && error.name !== 'AbortError') {
      controller.abort(); status.textContent = `WMTS error: ${error.message}`; setTraceDetail(error.message, viewport.sequence);
    }
  }
}

function visibleWmtsTile(tiles, key, url) {
  let pending = tiles.requests.get(key);
  if (pending) return pending;
  let queued;
  pending = new Promise((resolve, reject) => { queued = { key, url, resolve, reject }; });
  pending.controller = new AbortController();
  queued.request = pending;
  tiles.queue.push(queued);
  pending.done = false;
  tiles.requests.set(key, pending);
  const pump = () => {
    while (tiles.active < wmtsConcurrency && tiles.queue.length) {
      const job = tiles.queue.shift();
      const request = job.request;
      if (tiles.controller.signal.aborted || request.controller.signal.aborted || !tiles.visible.has(job.key)) {
        job.reject(new DOMException('Tile left the viewport', 'AbortError'));
        if (tiles.requests.get(job.key) === request) tiles.requests.delete(job.key);
        continue;
      }
      tiles.active += 1;
      const abort = () => request.controller.abort();
      tiles.controller.signal.addEventListener('abort', abort, { once: true });
      void fetch(job.url, { signal: request.controller.signal }).then(async response => {
        if (!response.ok) {
          const body = await response.text();
          let detail = body;
          try { detail = JSON.parse(body).message || JSON.parse(body).code || body; } catch {}
          throw new Error(detail || `WMTS GetTile returned HTTP ${response.status}`);
        }
        return { response, blob: await response.blob() };
      }).then(job.resolve, error => {
        if (tiles.requests.get(job.key) === request) tiles.requests.delete(job.key);
        job.reject(error);
      }).finally(() => {
        tiles.controller.signal.removeEventListener('abort', abort);
        if (tiles.requests.get(job.key) === request) {
          request.done = true;
          if (!tiles.visible.has(job.key)) tiles.requests.delete(job.key);
        }
        tiles.active -= 1;
        pump();
      });
    }
  };
  pump();
  return pending;
}

let requestedVisibleTiles = '';
function scheduleViewport() {
  if (activeViewport) {
    const edge = RIVER_TILE_SIZE * 2 ** activeViewport.zoom;
    const signature = `${activeRenderMode}:${activeViewport.zoom}:${Math.floor(activeViewport.x / edge)}:${Math.floor(activeViewport.y / edge)}:${Math.floor((activeViewport.x + activeViewport.width - 1) / edge)}:${Math.floor((activeViewport.y + activeViewport.height - 1) / edge)}`;
    if (signature !== requestedVisibleTiles) { requestedVisibleTiles = signature; requestViewport({ eager: deliveryMethod.value === 'progressive' }); }
    return;
  }
}

function panBy(deltaX, deltaY) {
  if (!activeViewport || (deltaX === 0 && deltaY === 0)) return false;
  const previousViewport = activeViewport;
  const layout = layoutFor(activeViewport);
  const nextViewport = viewportAt(
    activeViewport.x + activeViewport.width / 2 - deltaX * activeViewport.width / layout.stageWidth,
    activeViewport.y + activeViewport.height / 2 - deltaY * activeViewport.height / layout.stageHeight,
    activeViewport.zoom,
  );
  // Pointer movement at a source boundary can clamp to the same viewport.
  // Do not restart decoding unless the reference-grid view actually changed.
  if (nextViewport.x === previousViewport.x && nextViewport.y === previousViewport.y) return false;
  activeViewport = nextViewport;
  presentViewport();
  renderer.reposition(activeViewport, layoutFor(activeViewport));
  scheduleViewport();
  return true;
}

function zoomAt(clientX, clientY, direction) {
  if (!activeViewport || direction === 0) return;
  const rect = stage.getBoundingClientRect();
  const fractionX = clamp((clientX - rect.left) / rect.width, 0, 1);
  const fractionY = clamp((clientY - rect.top) / rect.height, 0, 1);
  const anchorX = activeViewport.x + activeViewport.width * fractionX;
  const anchorY = activeViewport.y + activeViewport.height * fractionY;
  const zoom = clamp(activeViewport.zoom + direction, 0, maxZoom());
  if (zoom === activeViewport.zoom) return;
  activeViewport = viewportAtAnchor(anchorX, anchorY, fractionX, fractionY, zoom);
  presentViewport();
  scheduleViewport();
}

function connect() {
  const socketOrigin = new URL(apiBase);
  socketOrigin.protocol = socketOrigin.protocol === 'https:' ? 'wss:' : 'ws:';
  session = new WebSocket(`${socketOrigin.toString().replace(/\/$/, '')}/ws`);
  session.binaryType = 'arraybuffer';
  session.onopen = async () => {
    if (!catalogInitialized) {
      catalogInitialized = true;
      try { await loadS3Sources(); } catch (error) { console.warn("S3 sources unavailable:", error); }
      void refreshAvailableAssets();
    }
    try { await refreshViewerOptions(); } catch (error) { console.warn('River driver/format discovery failed:', error); }
    await loadAsset({ preserve: true });

  };
  session.onclose = () => { status.textContent = 'disconnected; reconnecting…'; setTimeout(connect, 750); };
  session.onerror = () => { status.textContent = 'socket error'; };
  session.onmessage = async ({ data }) => {
    if (typeof data === 'string') handleControl(JSON.parse(data));
    else {
      try { await handleTileFrame(data); }
      catch (error) { status.textContent = `Tile display failed: ${error.message}`; }
    }
  };
}

function handleControl(message) {
  if (deliveryMethod.value !== 'progressive' || (message.sequence != null && message.sequence !== latestSequence)) return;
  if (message.type === 'accepted') {
    acceptedTrace = message.traceId;
    status.textContent = 'streaming decoded tiles';
    setTraceDetail(`session: ${message.sessionId}\ntrace: ${message.traceId}\nsequence: ${message.sequence}\nframes: 0`);
  } else if (message.type === 'error') {
    status.textContent = `River error: ${message.message || message.code}`;
    setTraceDetail(`error ${message.code}: ${message.message}`);
  }
}

async function handleTileFrame(buffer) {
  const bytes = new Uint8Array(buffer);
  const view = new DataView(buffer);
  if (new TextDecoder().decode(bytes.slice(0, 4)) !== 'RIVR') throw new Error('unknown River binary frame');
  const headerLength = view.getUint32(4, true);
  const header = JSON.parse(new TextDecoder().decode(bytes.slice(8, 8 + headerLength)));
  if (!activeViewport || header.sequence !== activeViewport.sequence || header.sequence !== latestSequence || deliveryMethod.value !== 'progressive') return;
  const receivedAt = performance.now();
  const payload = bytes.slice(8 + headerLength);
  const started = performance.now();
  const bitmap = await createImageBitmap(new Blob([payload], { type: header.contentType }));
  if (!activeViewport || header.sequence !== activeViewport.sequence || header.sequence !== latestSequence || deliveryMethod.value !== 'progressive') { bitmap.close(); return; }
  const decodeMs = performance.now() - started;
  const drawing = renderer.draw(header, bitmap, activeViewport, layoutFor(activeViewport));
  bitmap.close();
  if (drawing.ignored) return;
  const submittedAt = performance.now();
  const presentedAt = await drawing.presented;
  if (!activeViewport || header.sequence !== activeViewport.sequence || header.sequence !== latestSequence || deliveryMethod.value !== 'progressive') return;
  const uploadMs = drawing.uploadMs;
  const frameWaitMs = Math.max(0, presentedAt - submittedAt);
  const timing = header.serverTiming ?? {};
  const encodedFormat = (header.contentType?.split('/')[1] ?? 'webp').toUpperCase();
  const workerQueueMs = Number(timing.workerQueueMs) || 0;
  const decoderRoute = timing.decoderRoute || 'unknown';
  const serverDecodeMs = Number(timing.decodeMs) || 0;
  const serverEncodeMs = Number(timing.encodeMs) || 0;
  const serverElapsedMs = Number(timing.elapsedMs) || 0;
  const cacheMs = Number(timing.sourceCacheMs) || 0;
  const cacheStatus = timing.sourceCacheStatus || 'unknown';
  const requestAt = requestStartedAt.get(header.sequence) ?? receivedAt;
  const wireMs = Math.max(0, receivedAt - requestAt - serverElapsedMs);
  const serverOtherMs = Math.max(0, serverElapsedMs - workerQueueMs - serverDecodeMs - serverEncodeMs - cacheMs);
  const totalMs = presentedAt - requestAt;
  const latencyDetail = [
    `server queue ${ms(workerQueueMs)}`,
    `${decoderRoute} JPEG 2000 decode ${ms(serverDecodeMs)}`,
    `${encodedFormat} encode ${ms(serverEncodeMs)}`,
    `source cache ${cacheStatus}, foreground ${ms(cacheMs)}`,
    `server other ${ms(serverOtherMs)}`,
    `transport/dispatch estimate ≈${ms(wireMs)}`,
    `browser decode ${ms(decodeMs)}`,
    `WebGL ${ms(uploadMs)}`,
    `frame wait ${ms(frameWaitMs)}`,
    `end-to-end ${ms(totalMs)}`,
  ].join('; ');
  renderLatencyTrace({
    pass: header.pass,
    tile: `${header.tileX},${header.tileY}`,
    totalMs,
    detail: latencyDetail,
    segments: [
      ['queue', workerQueueMs], ['cache', cacheMs], ['decode', serverDecodeMs], ['encode', serverEncodeMs],
      ['server', serverOtherMs], ['wire', wireMs], ['browser', decodeMs], ['gl', uploadMs], ['paint', frameWaitMs],
    ],
  });
  canvas.classList.add('is-rendering');
  tileCount += 1;
  setTraceDetail([
    `driver: ${decoderDriver.selectedOptions[0].textContent}`,
    `method: progressive WebSocket; format: ${encodedFormat}`,
    `session: ${header.sessionId}`,
    `trace: ${header.traceId || acceptedTrace}`,
    `sequence: ${header.sequence}; frames: ${tileCount}`,
    `last: ${header.pass} tile ${header.tileX},${header.tileY}; source layers: ${header.sourceLayers}`,
    `server: ${decoderRoute}; queue ${ms(workerQueueMs)}; JPEG 2000 decode ${ms(serverDecodeMs)}; ${encodedFormat} encode ${ms(serverEncodeMs)}; total ${ms(serverElapsedMs)}`,
    `S3 source: ${((Number(timing.sourceReadBytes) || 0) / 1024).toFixed(1)} KiB fetched; ${timing.sourceRangeRequests || 0} ranges; ${((Number(timing.sourceCacheHitBytes) || 0) / 1024).toFixed(1)} KiB reused`,
    `source cache: ${cacheStatus}; foreground ${ms(cacheMs)}${cacheStatus === 'warming' ? '; verified staging runs in the background' : ''}`,
    `client: transport/dispatch estimate ≈${ms(wireMs)}; browser decode ${ms(decodeMs)}; WebGL upload ${ms(uploadMs)}; frame wait ${ms(frameWaitMs)}; frame submitted ${ms(totalMs)}`,
  ].join('\n'));
}

function createRenderer(target) {
  const gl = target.getContext('webgl2', { alpha: false });
  if (!gl) throw new Error('River needs WebGL2');
  const program = linkProgram(gl, `#version 300 es
    in vec2 position; in vec2 uv; out vec2 vUv;
    void main() { vUv = uv; gl_Position = vec4(position, 0.0, 1.0); }`, `#version 300 es
    precision mediump float; in vec2 vUv; out vec4 color;
    uniform sampler2D tile; void main() { color = texture(tile, vUv); }`);
  const buffer = gl.createBuffer();
  const tiles = new Map();
  let renderedZoom = null;
  let renderedAsset = null;
  let frame = null;
  let frameTarget = null;
  let frameWaiters = [];
  gl.useProgram(program);
  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
  const position = gl.getAttribLocation(program, 'position');
  const uv = gl.getAttribLocation(program, 'uv');
  const drawTile = (header, texture, viewport, layout) => {
    const sourceTile = header.sourceTileEdge ?? RIVER_TILE_SIZE * layout.scale;
    // Adjacent WebGL quads can leave a sub-pixel uncovered after the
    // source-to-screen transform.  Draw a tiny clamped edge halo so joins
    // overlap rather than exposing the cleared framebuffer as black seams.
    const seamGuard = 0.75;
    const left = (header.tileX * sourceTile - viewport.x) / layout.scale - seamGuard;
    const top = (header.tileY * sourceTile - viewport.y) / layout.scale - seamGuard;
    const right = left + header.outputWidth + seamGuard * 2;
    const bottom = top + header.outputHeight + seamGuard * 2;
    const x0 = (layout.offsetX + left) / layout.stageWidth * 2 - 1;
    const x1 = (layout.offsetX + right) / layout.stageWidth * 2 - 1;
    const y0 = 1 - (layout.offsetY + top) / layout.stageHeight * 2;
    const y1 = 1 - (layout.offsetY + bottom) / layout.stageHeight * 2;
    const vertices = new Float32Array([x0, y1, 0, 1, x1, y1, 1, 1, x0, y0, 0, 0, x0, y0, 0, 0, x1, y1, 1, 1, x1, y0, 1, 0]);
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer); gl.bufferData(gl.ARRAY_BUFFER, vertices, gl.STREAM_DRAW);
    gl.enableVertexAttribArray(position); gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 16, 0);
    gl.enableVertexAttribArray(uv); gl.vertexAttribPointer(uv, 2, gl.FLOAT, false, 16, 8);
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.drawArrays(gl.TRIANGLES, 0, 6);
  };
  const redraw = (viewport, layout) => {
    gl.clearColor(0.05, 0.07, 0.09, 1); gl.clear(gl.COLOR_BUFFER_BIT);
    for (const [key, tile] of tiles) {
      const edge = tile.header.sourceTileEdge ?? RIVER_TILE_SIZE * layout.scale;
      const x = tile.header.tileX * edge, y = tile.header.tileY * edge;
      if (x >= viewport.x + viewport.width || y >= viewport.y + viewport.height || x + tile.header.outputWidth * layout.scale <= viewport.x || y + tile.header.outputHeight * layout.scale <= viewport.y) {
        gl.deleteTexture(tile.texture); tiles.delete(key);
      }
    }
    // Preview quads are always below detail, regardless of decode order.
    for (const tile of tiles.values()) if (tile.header.preview) drawTile(tile.header, tile.texture, viewport, layout);
    for (const tile of tiles.values()) if (!tile.header.preview) drawTile(tile.header, tile.texture, viewport, layout);
  };
  const scheduleDraw = (viewport, layout) => {
    frameTarget = { viewport, layout };
    if (frame !== null) return;
    frame = requestAnimationFrame(() => {
      frame = null;
      const target = frameTarget;
      frameTarget = null;
      redraw(target.viewport, target.layout);
      const submittedAt = performance.now();
      const waiting = frameWaiters; frameWaiters = [];
      for (const resolve of waiting) resolve(submittedAt);
    });
  };
  return {
    clear() {
      for (const tile of tiles.values()) gl.deleteTexture(tile.texture);
      tiles.clear();
      if (frame !== null) cancelAnimationFrame(frame);
      frame = null; frameTarget = null;
      for (const resolve of frameWaiters) resolve(performance.now());
      frameWaiters = []; renderedAsset = null; renderedZoom = null;
      gl.clearColor(0.05, 0.07, 0.09, 1); gl.clear(gl.COLOR_BUFFER_BIT);
    },
    resize(width, height) {
      if (target.width === width && target.height === height) return;
      target.width = width; target.height = height; gl.viewport(0, 0, width, height);
    },
    begin(viewport, layout, force = false) {
      if (!force && renderedAsset === viewport.assetId && renderedZoom === viewport.zoom) {
        scheduleDraw(viewport, layout);
        return;
      }
      for (const tile of tiles.values()) gl.deleteTexture(tile.texture);
      tiles.clear();
      if (frame !== null) cancelAnimationFrame(frame);
      frame = null; frameTarget = null;
      for (const resolve of frameWaiters) resolve(performance.now());
      frameWaiters = [];
      if (renderedAsset !== viewport.assetId) {
        gl.clearColor(0.05, 0.07, 0.09, 1); gl.clear(gl.COLOR_BUFFER_BIT);
      }
      renderedAsset = viewport.assetId;
      renderedZoom = viewport.zoom;
      // Keep the last complete frame on screen until the first tile at the
      // new reduction arrives; this avoids a black flash while zooming.
    },
    reposition(viewport, layout) {
      if (renderedZoom === viewport.zoom) scheduleDraw(viewport, layout);
    },
    hasTile(row, col, reduction, preview) {
      const tile = tiles.get(`${preview ? `overview:${RIVER_TILE_SIZE * 2 ** reduction}:` : ''}${col}:${row}`);
      return !!tile && (preview || tile.quality >= 2);
    },
    draw(header, bitmap, viewport, layout) {
      const started = performance.now();
      const key = `${header.preview ? `overview:${header.sourceTileEdge}:` : ''}${header.tileX}:${header.tileY}`;
      const previous = tiles.get(key);
      const quality = header.preview || header.pass === 'preview' ? 0 : header.pass === 'gain' ? 1 : 2;
      if (previous && (previous.quality > quality || previous.sourceLayers > header.sourceLayers)) return { ignored: true, uploadMs: 0, presented: Promise.resolve(performance.now()) };
      const texture = previous?.texture ?? gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, texture);
      if (!previous) {
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      }
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bitmap);
      tiles.set(key, { header, texture, sourceLayers: header.sourceLayers, quality });
      const presented = new Promise(resolve => frameWaiters.push(resolve));
      scheduleDraw(viewport, layout);
      return { uploadMs: performance.now() - started, presented };
    },
  };
}

function linkProgram(gl, vertexSource, fragmentSource) {
  const compile = (type, source) => { const shader = gl.createShader(type); gl.shaderSource(shader, source); gl.compileShader(shader); if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(shader)); return shader; };
  const program = gl.createProgram(); gl.attachShader(program, compile(gl.VERTEX_SHADER, vertexSource)); gl.attachShader(program, compile(gl.FRAGMENT_SHADER, fragmentSource)); gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(program));
  return program;
}

const s3SourceList = document.querySelector('#s3-source-list');
const s3ObjectList = document.querySelector('#s3-object-list');
const s3Status = document.querySelector('#s3-status');
function s3Headers(json = false) {
  return json ? { 'Content-Type': 'application/json' } : {};
}
async function loadS3Sources() {
  const response = await fetch(`${apiBase}/api/s3/sources`);
  const body = await response.json();
  if (!response.ok) throw new Error(body.message ?? body.code ?? `HTTP ${response.status}`);
  const previous = s3SourceList.value;
  s3SourceList.replaceChildren(...body.map((source) => new Option(`${source.name} (${source.id})`, source.id)));
  if (body.some((source) => source.id === previous)) s3SourceList.value = previous;
  const selectedCatalog = catalogSource.value;
  catalogSource.replaceChildren(new Option('All connected S3 sources', 'all'), new Option('Local assets', ''), ...body.map(source => new Option(source.name, source.id)));
  catalogSource.value = selectedCatalog === '' || selectedCatalog === 'all' || body.some(source => source.id === selectedCatalog) ? selectedCatalog : 'all';
  if (!body.length && catalogSource.value === 'all') catalogSource.value = '';
  return body;
}
document.querySelector('#s3-add-source').addEventListener('click', async () => {
  s3Status.textContent = 'Saving S3 connection…';
  const value = (id) => document.querySelector(`#${id}`).value;
  try {
    const response = await fetch(`${apiBase}/api/s3/sources`, {
      method: 'POST', headers: s3Headers(true),
      body: JSON.stringify({
        id: value('s3-id'), name: value('s3-name'), endpoint: value('s3-endpoint') || null,
        bucket: value('s3-bucket'), region: value('s3-region'), prefix: value('s3-prefix'),
        accessKeyId: value('s3-access-key'), secretAccessKey: value('s3-secret-key'),
        sessionToken: value('s3-session-token') || null,
        allowHttp: document.querySelector('#s3-allow-http').checked,
        virtualHostedStyle: !document.querySelector('#s3-path-style').checked,
        maxRetries: Number(document.querySelector('#s3-max-retries')?.value ?? 2),
      }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.message ?? body.code ?? `HTTP ${response.status}`);
    document.querySelector('#s3-secret-key').value = '';
    document.querySelector('#s3-session-token').value = '';
    await loadS3Sources(); s3SourceList.value = body.id;
    catalogSource.value = body.id;
    await refreshAvailableAssets();
    resetS3Browse();
    s3Status.textContent = `Saved ${body.name}. The connection is persisted on this server.`;
  } catch (error) { s3Status.textContent = `S3 setup failed: ${error.message}`; }
});
document.querySelector('#s3-remove-source').addEventListener('click', async () => {
  s3Status.textContent = 'Removing runtime connection…';
  try {
    if (!s3SourceList.value) throw new Error('Choose a configured source.');
    const response = await fetch(`${apiBase}/api/s3/sources/${encodeURIComponent(s3SourceList.value)}`, { method: 'DELETE', headers: s3Headers() });
    if (!response.ok) { const body = await response.json(); throw new Error(body.message ?? body.code ?? `HTTP ${response.status}`); }
    resetS3Browse(); await loadS3Sources();
    await refreshAvailableAssets();
    s3Status.textContent = 'Runtime connection removed. Previously loaded assets remain available until River restarts.';
  } catch (error) { s3Status.textContent = `Could not remove source: ${error.message}`; }
});
let s3Cursor = null;
let s3Browse;
const s3Next = document.querySelector('#s3-next-objects');
const s3Search = document.querySelector('#s3-object-search');
function resetS3Browse() {
  s3Browse?.abort();
  s3Cursor = null;
  s3Next.disabled = true;
  s3ObjectList.replaceChildren();
}
async function browseS3Objects(next = false) {
  if (!next) resetS3Browse();
  if (!s3SourceList.value) {
    s3Status.textContent = 'Choose a configured source.';
    return;
  }
  s3Browse?.abort();
  const controller = new AbortController();
  s3Browse = controller;
  s3Status.textContent = 'Listing S3 objects…';
  s3Next.disabled = true;
  try {
    const params = new URLSearchParams({ limit: '100' });
    if (s3Search.value.trim()) params.set('keyPrefix', s3Search.value.trim());
    if (next && s3Cursor) params.set('cursor', s3Cursor);
    const response = await fetch(`${apiBase}/api/s3/sources/${encodeURIComponent(s3SourceList.value)}/objects?${params}`,
      { signal: controller.signal });
    const body = await response.json();
    if (controller.signal.aborted) return;
    if (!response.ok) throw new Error(body.message ?? body.code ?? `HTTP ${response.status}`);
    s3Cursor = body.nextCursor;
    s3Next.disabled = !s3Cursor;
    s3ObjectList.replaceChildren(...body.objects.map(object => new Option(`${object.key} (${object.size} bytes)`, object.key)));
    s3Status.textContent = `${body.objects.length} matching images on this page. ${s3Cursor ? 'Select Next page to keep browsing.' : 'End of results.'}`;
  } catch (error) {
    if (controller.signal.aborted) return;
    s3Next.disabled = !s3Cursor;
    s3Status.textContent = `S3 listing failed: ${error.message}`;
  }
}
document.querySelector('#s3-refresh-objects').addEventListener('click', () => { void browseS3Objects(); });
s3Next.addEventListener('click', () => { void browseS3Objects(true); });
s3SourceList.addEventListener('change', () => { void browseS3Objects(); });
s3Search.addEventListener('input', () => {
  resetS3Browse();
  s3Status.textContent = 'Select Browse objects to search.';
});
s3Search.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') { event.preventDefault(); void browseS3Objects(); }
});
document.querySelector('#s3-load-object').addEventListener('click', async () => {
  s3Status.textContent = 'Reading image headers…';
  try {
    if (!s3SourceList.value || !s3ObjectList.value) throw new Error('Choose a configured source and JP2 object.');
    const response = await fetch(`${apiBase}/api/s3/sources/${encodeURIComponent(s3SourceList.value)}/assets`, {
      method: 'POST', headers: s3Headers(true),
      body: JSON.stringify({ key: s3ObjectList.value }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.message ?? body.code ?? `HTTP ${response.status}`);
    inputs.asset.value = body.assetId;
    await loadAsset();
    s3Status.textContent = `Image available; ${body.bytesFetched ?? 0} header bytes read. Viewport data is fetched on demand.`;
  } catch (error) { s3Status.textContent = `JP2 load failed: ${error.message}`; }
});

stage.addEventListener('pointerdown', (event) => {
  if (!activeViewport) return;
  drag = { pointerId: event.pointerId, x: event.clientX, y: event.clientY };
  stage.setPointerCapture(event.pointerId); stage.classList.add('is-dragging'); event.preventDefault();
});
stage.addEventListener('pointermove', (event) => {
  if (!drag || event.pointerId !== drag.pointerId) return;
  panBy(event.clientX - drag.x, event.clientY - drag.y);
  drag = { ...drag, x: event.clientX, y: event.clientY };
});
for (const name of ['pointerup', 'pointercancel']) {
  stage.addEventListener(name, (event) => {
    if (!drag || event.pointerId !== drag.pointerId) return;
    stage.releasePointerCapture(event.pointerId); stage.classList.remove('is-dragging'); drag = null;
  });
}
stage.addEventListener('wheel', (event) => { event.preventDefault(); zoomAt(event.clientX, event.clientY, Math.sign(event.deltaY)); }, { passive: false });
decoderDriver.addEventListener('change', () => requestViewport());
deliveryMethod.addEventListener('change', () => {
  applyAssetDecoderCapabilities();
  requestViewport();
});
rasterFormat.addEventListener('change', () => requestViewport());
boundaryColor.addEventListener('input', () => { if (activeViewport) drawTileBoundaries(activeViewport); });
boundaryEnabled.addEventListener('change', () => { if (activeViewport) drawTileBoundaries(activeViewport); });
boundaryWidth.addEventListener('input', () => { document.querySelector('#boundary-width-value').textContent = `${boundaryWidth.value} px`; if (activeViewport) drawTileBoundaries(activeViewport); });
catalogSearchMode.disabled = !catalogSource.value;
function searchCatalog() {
  if (!catalogSource.value) { renderCatalog(); return; }
  catalogAbort?.abort();
  ++catalogGeneration;
  catalogDraft = true;
  catalogLoading = false;
  catalogNext.disabled = true;
  catalogPrevious.disabled = true;
  renderCatalog();
}
catalogSearch.addEventListener('input', searchCatalog);
catalogSearchMode.addEventListener('change', searchCatalog);
catalogPageSize.addEventListener('change', () => {
  catalogAbort?.abort();
  ++catalogGeneration;
  catalogDraft = true;
  catalogLoading = false;
  catalogNext.disabled = true;
  catalogPrevious.disabled = true;
  renderCatalog();
});
catalogSearch.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') { event.preventDefault(); void refreshAvailableAssets(); }
});
catalogSort.addEventListener('change', renderCatalog);
catalogDirection.addEventListener('click', () => {
  catalogDescending = !catalogDescending;
  catalogDirection.textContent = catalogDescending ? '↓' : '↑';
  const next = catalogDescending ? 'Sort ascending' : 'Sort descending';
  catalogDirection.title = next; catalogDirection.setAttribute('aria-label', next);
  renderCatalog();
});
document.querySelector('#catalog-refresh').addEventListener('click', () => { void refreshAvailableAssets(); });
new ResizeObserver(() => {
  const { width, height } = stageSize();
  const nextSize = `${width}x${height}`;
  if (nextSize === observedStageSize) return;
  observedStageSize = nextSize;
  if (assetManifest && activeViewport) void loadAsset({ preserve: true });
}).observe(stage);
requestButton.addEventListener('click', () => {
  if (activeViewport?.assetId !== inputs.asset.value.trim()) { void loadAsset(); return; }
  const zoom = Number(inputs.zoom.value);
  activeViewport = viewportAt(Number(inputs.x.value) + activeViewport.width / 2, Number(inputs.y.value) + activeViewport.height / 2, Number.isFinite(zoom) ? zoom : activeViewport.zoom);
  presentViewport(); requestViewport();
});
fitButton.addEventListener('click', () => { if (assetManifest) { activeViewport = fitViewport(); presentViewport(); requestViewport(); } });
randomButton.addEventListener('click', () => {
  if (!assetManifest) return;
  void (async () => {
    await refreshAvailableAssets();
    const alternatives = availableAssets.filter((asset) => asset.assetId !== activeViewport?.assetId);
    const selected = (alternatives.length ? alternatives : availableAssets)[Math.floor(Math.random() * (alternatives.length ? alternatives.length : availableAssets.length))];
    if (selected && selected.assetId !== activeViewport?.assetId) {
      inputs.asset.value = selected.assetId;
      await loadAsset({ randomize: true });
      return;
    }
    activeViewport = randomViewport();
    presentViewport();
    requestViewport();
  })();
});

connect();

const encodingControls = document.querySelector('#encoding-controls');
const encodingMethod = document.querySelector('#encoding-webp-method');
const encodingStatus = document.querySelector('#encoding-status');
encodingControls?.addEventListener('toggle', async () => {
  if (!encodingControls.open) return;
  try {
    const response = await fetch(`${apiBase}/api/settings/encoding`);
    if (!response.ok) throw new Error(await responseError(response));
    encodingMethod.value = String((await response.json()).webpMethod);
    encodingStatus.textContent = '';
  } catch (error) { encodingStatus.textContent = error.message; }
});
document.querySelector('#encoding-save')?.addEventListener('click', async () => {
  encodingStatus.textContent = 'Saving…';
  try {
    const response = await fetch(`${apiBase}/api/settings/encoding`, {method:'PUT', headers:s3Headers(true),
      body:JSON.stringify({webpMethod:Number(encodingMethod.value)})});
    if (!response.ok) throw new Error(await responseError(response));
    encodingMethod.value = String((await response.json()).webpMethod);
    encodingStatus.textContent = 'Saved. New renders use this setting, including after a server restart.';
  } catch (error) { encodingStatus.textContent = error.message; }
});

async function responseError(response) {
  const text = await response.text();
  try { const error = JSON.parse(text); return `${error.message || error.code || response.status}${error.requestId ? ` (request ${error.requestId})` : ''}`; }
  catch { return text || `Request failed (HTTP ${response.status})`; }
}

// Runtime configuration is intentionally unauthenticated in development River.
async function configurationRequest(path, value) {
  const response = await fetch(`${apiBase}${path}`, value === undefined ? {} : { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value) });
  const result = await response.json();
  if (!response.ok) throw new Error(result.message || result.code || `HTTP ${response.status}`);
  return result;
}
function configurationAction(id, action) {
  document.querySelector(id).addEventListener('click', async () => {
    const status = document.querySelector('#configuration-status');
    try { await action(); status.textContent = 'Done.'; } catch (error) { status.textContent = error.message; }
  });
}
async function refreshReadPolicy() {
  const result = await configurationRequest('/api/settings/read-policy');
  document.querySelector('#read-policy-mode').value = result.settings.mode;
  const transport = document.querySelector('#read-policy-transport');
  if (transport) {
    transport.value = result.settings.serialTransport || 'sdk';
    transport.disabled = !Object.prototype.hasOwnProperty.call(result.settings, 'serialTransport');
  }
  const sourceLimit = document.querySelector('#read-policy-source-limit');
  if (sourceLimit) { sourceLimit.value = result.settings.maxSourceRequests ?? 16; sourceLimit.disabled = result.settings.maxSourceRequests === undefined; }
  document.querySelector('#read-policy-observations').textContent = JSON.stringify(result.sources, null, 2);
  return result.settings;
}
configurationAction('#read-policy-refresh', refreshReadPolicy);
configurationAction('#read-policy-save', async () => {
  const selected = document.querySelector('#read-policy-mode').value;
  const current = await configurationRequest('/api/settings/read-policy');
  const settings = { ...current.settings, mode: selected };
  if (Object.prototype.hasOwnProperty.call(current.settings, 'serialTransport')) settings.serialTransport = document.querySelector('#read-policy-transport')?.value || current.settings.serialTransport;
  const sourceLimit = document.querySelector('#read-policy-source-limit');
  if (sourceLimit && current.settings.maxSourceRequests !== undefined) settings.maxSourceRequests = Number(sourceLimit.value);
  await configurationRequest('/api/settings/read-policy', settings);
  await refreshReadPolicy();
});
configurationAction('#configuration-export', async () => {
  document.querySelector('#configuration-json').value = JSON.stringify(await configurationRequest('/api/settings/configuration'), null, 2);
});
configurationAction('#configuration-import', async () => {
  await configurationRequest('/api/settings/configuration', JSON.parse(document.querySelector('#configuration-json').value));
  await refreshReadPolicy(); await loadS3Sources();
});

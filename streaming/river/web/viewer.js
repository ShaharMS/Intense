const stage = document.querySelector('#stage');
const canvas = document.querySelector('#viewport');
const overview = document.querySelector('#overview');
const status = document.querySelector('#status');
const trace = document.querySelector('#trace');
const requestButton = document.querySelector('#request');
const fitButton = document.querySelector('#fit');
const inputs = Object.fromEntries(['asset', 'x', 'y', 'width', 'height', 'zoom'].map((id) => [id, document.querySelector(`#${id}`)]));
const apiOrigin = new URL(window.RIVER_API_ORIGIN ?? location.origin);
const apiBase = apiOrigin.toString().replace(/\/$/, '');
document.querySelector('#api-origin').textContent = apiBase;

let assetManifest = null;
let activeViewport = null;
let session = null;
let latestSequence = 0;
let acceptedTrace = null;
let tileCount = 0;
let drag = null;
let requestTimer = null;
let nextRequestAt = 0;
let observedStageSize = '';
const renderer = createRenderer(canvas);
const DRAG_REQUEST_INTERVAL_MS = 60;

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

function presentViewport() {
  if (!assetManifest || !activeViewport) return;
  const layout = layoutFor(activeViewport);
  overview.style.width = `${assetManifest.width / layout.scale}px`;
  overview.style.height = `${assetManifest.height / layout.scale}px`;
  overview.style.transform = `translate(${layout.offsetX - activeViewport.x / layout.scale}px, ${layout.offsetY - activeViewport.y / layout.scale}px)`;
  // The overview is only a fallback for unavailable sources.  It must never
  // surface during a fast-path drag: it represents a different asset.
  overview.hidden = !isLimited();
  canvas.classList.toggle('is-rendering', !isLimited());
  renderer.resize(layout.stageWidth, layout.stageHeight);
  observedStageSize = `${layout.stageWidth}x${layout.stageHeight}`;
  syncControls();
}

function isLimited() {
  return assetManifest?.capability?.status === 'limited';
}

async function loadAsset({ preserve = false } = {}) {
  const assetId = inputs.asset.value.trim();
  if (!assetId) return;
  status.textContent = 'checking asset manifest…';
  try {
    const response = await fetch(`${apiBase}/api/assets/${encodeURIComponent(assetId)}/manifest`);
    if (!response.ok) throw new Error(`manifest request returned HTTP ${response.status}`);
    assetManifest = await response.json();
    const centerX = preserve && activeViewport?.assetId === assetId ? activeViewport.x + activeViewport.width / 2 : assetManifest.width / 2;
    const centerY = preserve && activeViewport?.assetId === assetId ? activeViewport.y + activeViewport.height / 2 : assetManifest.height / 2;
    const zoom = preserve && activeViewport?.assetId === assetId ? activeViewport.zoom : maxZoom();
    activeViewport = viewportAt(centerX, centerY, zoom);
    presentViewport();
    requestViewport();
  } catch (error) {
    status.textContent = `River unavailable: ${error.message}`;
  }
}

function requestViewport() {
  if (!assetManifest || !activeViewport) return;
  if (isLimited()) {
    status.textContent = 'overview mode — drag to pan; scroll to zoom';
    trace.textContent = [
      `asset: ${activeViewport.assetId}`,
      `reference grid: ${assetManifest.width} × ${assetManifest.height}`,
      `viewport: ${activeViewport.x},${activeViewport.y} ${activeViewport.width} × ${activeViewport.height}`,
      `zoom: 1:${2 ** activeViewport.zoom} source pixels per screen pixel`,
      ...assetManifest.capability.reasons.map((reason) => `limited: ${reason}`),
    ].join('\n');
    return;
  }
  if (!session || session.readyState !== WebSocket.OPEN) return;
  const viewport = { ...activeViewport, sequence: ++latestSequence };
  activeViewport = viewport;
  tileCount = 0;
  renderer.begin(viewport, layoutFor(viewport));
  session.send(JSON.stringify({ type: 'viewport', viewport }));
  status.textContent = 'decoding JPEG 2000 tiles…';
  trace.textContent = `sequence ${viewport.sequence} requested\nwaiting for OpenJPEG tiles…`;
}

function scheduleViewport() {
  const delay = Math.max(0, nextRequestAt - performance.now());
  if (delay === 0) {
    if (requestTimer !== null) {
      clearTimeout(requestTimer);
      requestTimer = null;
    }
    nextRequestAt = performance.now() + DRAG_REQUEST_INTERVAL_MS;
    requestViewport();
    return;
  }
  if (requestTimer !== null) return;
  requestTimer = setTimeout(() => {
    requestTimer = null;
    nextRequestAt = performance.now() + DRAG_REQUEST_INTERVAL_MS;
    requestViewport();
  }, delay);
}

function panBy(deltaX, deltaY) {
  const layout = layoutFor(activeViewport);
  activeViewport = viewportAt(
    activeViewport.x + activeViewport.width / 2 - deltaX * activeViewport.width / layout.stageWidth,
    activeViewport.y + activeViewport.height / 2 - deltaY * activeViewport.height / layout.stageHeight,
    activeViewport.zoom,
  );
  presentViewport();
  renderer.reposition(activeViewport, layoutFor(activeViewport));
  scheduleViewport();
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
  session.onopen = () => { void loadAsset({ preserve: true }); };
  session.onclose = () => { status.textContent = 'disconnected; reconnecting…'; setTimeout(connect, 750); };
  session.onerror = () => { status.textContent = 'socket error'; };
  session.onmessage = async ({ data }) => {
    if (typeof data === 'string') handleControl(JSON.parse(data));
    else await handleTileFrame(data);
  };
}

function handleControl(message) {
  if (message.type === 'accepted') {
    acceptedTrace = message.traceId;
    status.textContent = 'streaming decoded tiles';
    trace.textContent = `session: ${message.sessionId}\ntrace: ${message.traceId}\nsequence: ${message.sequence}\nframes: 0`;
  } else if (message.type === 'error') {
    status.textContent = `River error: ${message.code}`;
    trace.textContent = `error ${message.code}: ${message.message}`;
  }
}

async function handleTileFrame(buffer) {
  const bytes = new Uint8Array(buffer);
  const view = new DataView(buffer);
  if (new TextDecoder().decode(bytes.slice(0, 4)) !== 'RIVR') throw new Error('unknown River binary frame');
  const headerLength = view.getUint32(4, true);
  const header = JSON.parse(new TextDecoder().decode(bytes.slice(8, 8 + headerLength)));
  if (!activeViewport || header.sequence !== activeViewport.sequence) return;
  const payload = bytes.slice(8 + headerLength);
  const started = performance.now();
  const bitmap = await createImageBitmap(new Blob([payload], { type: header.contentType }));
  const decodeMs = performance.now() - started;
  const uploadMs = renderer.draw(header, bitmap, activeViewport, layoutFor(activeViewport));
  bitmap.close();
  overview.hidden = true;
  canvas.classList.add('is-rendering');
  tileCount += 1;
  trace.textContent = [
    `session: ${header.sessionId}`,
    `trace: ${header.traceId || acceptedTrace}`,
    `sequence: ${header.sequence}; frames: ${tileCount}`,
    `last: ${header.pass} tile ${header.tileX},${header.tileY}; source layers: ${header.sourceLayers}`,
    `browser decode: ${decodeMs.toFixed(2)} ms; WebGL upload/draw: ${uploadMs.toFixed(2)} ms`,
  ].join('\n');
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
  gl.useProgram(program);
  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
  const position = gl.getAttribLocation(program, 'position');
  const uv = gl.getAttribLocation(program, 'uv');
  const drawTile = (header, texture, viewport, layout) => {
    const sourceTile = 256 * layout.scale;
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
    for (const tile of tiles.values()) drawTile(tile.header, tile.texture, viewport, layout);
  };
  return {
    resize(width, height) {
      if (target.width === width && target.height === height) return;
      target.width = width; target.height = height; gl.viewport(0, 0, width, height);
    },
    begin(viewport, layout) {
      if (renderedZoom === viewport.zoom) {
        redraw(viewport, layout);
        return;
      }
      for (const tile of tiles.values()) gl.deleteTexture(tile.texture);
      tiles.clear();
      renderedZoom = viewport.zoom;
      // Keep the last complete frame on screen until the first tile at the
      // new reduction arrives; this avoids a black flash while zooming.
    },
    reposition(viewport, layout) {
      if (renderedZoom === viewport.zoom) redraw(viewport, layout);
    },
    draw(header, bitmap, viewport, layout) {
      const started = performance.now();
      const key = `${header.tileX}:${header.tileY}`;
      const previous = tiles.get(key);
      if (previous && previous.sourceLayers > header.sourceLayers) return performance.now() - started;
      const texture = previous?.texture ?? gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, texture);
      if (!previous) {
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      }
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bitmap);
      tiles.set(key, { header, texture, sourceLayers: header.sourceLayers });
      redraw(viewport, layout);
      return performance.now() - started;
    },
  };
}

function linkProgram(gl, vertexSource, fragmentSource) {
  const compile = (type, source) => { const shader = gl.createShader(type); gl.shaderSource(shader, source); gl.compileShader(shader); if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(shader)); return shader; };
  const program = gl.createProgram(); gl.attachShader(program, compile(gl.VERTEX_SHADER, vertexSource)); gl.attachShader(program, compile(gl.FRAGMENT_SHADER, fragmentSource)); gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(program));
  return program;
}

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

connect();

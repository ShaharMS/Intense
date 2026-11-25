const viewers = Number.parseInt(process.env.RIVER_VIEWERS ?? "10", 10);
const viewportWidth = Number.parseInt(process.env.RIVER_VIEWPORT_WIDTH ?? "1920", 10);
const viewportHeight = Number.parseInt(process.env.RIVER_VIEWPORT_HEIGHT ?? "1080", 10);
const server = process.env.RIVER_WS_URL ?? "ws://127.0.0.1:18080/ws";
const assetId = process.env.RIVER_ASSET_ID ?? "94a94669d839b13292340cbbdec6b96c81475eb1e1f6158c55245a7355437290";
const previewFrames = Math.ceil(viewportWidth / 512) * Math.ceil(viewportHeight / 512);

const samples = [];
const jobs = Array.from({ length: viewers }, (_, index) => new Promise((resolve, reject) => {
  const socket = new WebSocket(server);
  const started = performance.now();
  let receivedPreviewFrames = 0;
  const timer = setTimeout(() => reject(new Error(`viewer ${index} timed out`)), 5_000);

  socket.addEventListener("open", () => socket.send(JSON.stringify({
    type: "viewport",
    viewport: { assetId, sequence: index + 1, x: 0, y: 0, width: viewportWidth, height: viewportHeight, zoom: 0 },
  })));
  socket.addEventListener("error", reject);
  socket.addEventListener("message", async (event) => {
    if (typeof event.data === "string") return;
    const bytes = Buffer.from(await event.data.arrayBuffer());
    if (bytes.subarray(0, 4).toString("utf8") !== "RIVR") {
      reject(new Error("received an unknown binary frame"));
      return;
    }
    const headerLength = bytes.readUInt32LE(4);
    const header = JSON.parse(bytes.subarray(8, 8 + headerLength).toString("utf8"));
    if (header.pass !== "preview") return;
    receivedPreviewFrames += 1;
    if (receivedPreviewFrames === previewFrames) {
      clearTimeout(timer);
      samples.push(performance.now() - started);
      socket.close();
      resolve();
    }
  });
}));

await Promise.all(jobs);
const sorted = samples.sort((left, right) => left - right);
const p95Index = Math.ceil(sorted.length * 0.95) - 1;
console.log(JSON.stringify({
  benchmark: "river-openjpeg-websocket-preview",
  server,
  viewers,
  viewport: { width: viewportWidth, height: viewportHeight, zoom: 0 },
  previewFramesPerViewer: previewFrames,
  medianMs: Number(sorted[Math.floor((sorted.length - 1) / 2)].toFixed(3)),
  p95Ms: Number(sorted[p95Index].toFixed(3)),
  minMs: Number(sorted[0].toFixed(3)),
  maxMs: Number(sorted.at(-1).toFixed(3)),
  limitations: ["No browser WebP decode or WebGL upload", "No MinIO object read in this local fixture"],
}));

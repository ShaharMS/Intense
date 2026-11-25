"use strict";

const status = document.querySelector("#status");
const DETAIL_ENTER_HEIGHT = 35000;
const DETAIL_EXIT_HEIGHT = 45000;
const OVERVIEW_HEIGHT = 1500000;
const MINIMUM_HEIGHT = 2;

const transparentTile = document.createElement("canvas");
transparentTile.width = transparentTile.height = 1;

function riverImagery(id, layer) {
  const bounds = Cesium.Rectangle.fromDegrees(...layer.bounds);
  const mercator = layer.grid === "EPSG:3857";
  const tilingScheme = mercator
    ? new Cesium.WebMercatorTilingScheme()
    : new Cesium.GeographicTilingScheme();
  const provider = new Cesium.UrlTemplateImageryProvider({
    url: `/api/maps/${encodeURIComponent(id)}/{z}/{x}/{y}`,
    tilingScheme,
    maximumLevel: layer.maxZoom,
    credit: layer.attribution,
  });

  // A coverage rectangle aligned to the tile grid can make Cesium's terrain
  // fill tiles intersect an adjacent imagery tile with zero area.
  const requestImage = provider.requestImage.bind(provider);
  const tileBounds = new Cesium.Rectangle();
  const overlap = new Cesium.Rectangle();
  provider.requestImage = (x, y, level, request) => {
    if (level < layer.minZoom || !Cesium.Rectangle.intersection(
      bounds, tilingScheme.tileXYToRectangle(x, y, level, tileBounds), overlap,
    )) return Promise.resolve(transparentTile);
    return requestImage(x, y, level, request);
  };
  return provider;
}

async function start() {
  const response = await fetch("/api/maps", {cache: "no-store"});
  if (!response.ok) throw new Error(`River map catalog returned HTTP ${response.status}`);
  const {layers} = await response.json();
  if (!layers["blue-marble"]) throw new Error("Configure a Blue Marble layer in the River map catalog.");

  // River serves these tiles over a multiplexed connection. Let the transport
  // negotiate stream capacity while Cesium continues to cancel obsolete tiles.
  const riverServer = Cesium.RequestScheduler.getServerKey(location.origin);
  Cesium.RequestScheduler.requestsByServer[riverServer] = Infinity;

  const viewer = new Cesium.Viewer("globe", {
    baseLayer: false,
    baseLayerPicker: false,
    geocoder: false,
    homeButton: false,
    sceneModePicker: false,
    navigationHelpButton: false,
    infoBox: false,
    selectionIndicator: false,
    timeline: false,
    animation: false,
    fullscreenButton: false,
    requestRenderMode: true,
    maximumRenderTimeChange: Infinity,
    targetFrameRate: 30,
    msaaSamples: 1,
    skyBox: false,
    skyAtmosphere: false,
  });
  viewer.resolutionScale = 0.75;
  viewer.scene.globe.enableLighting = false;
  viewer.scene.globe.preloadAncestors = false;
  viewer.scene.globe.preloadSiblings = false;
  viewer.scene.globe.tileCacheSize = 0;
  viewer.scene.globe.loadingDescendantLimit = 8;
  viewer.scene.fog.enabled = false;

  const {LEFT_DRAG, RIGHT_DRAG, PINCH} = Cesium.CameraEventType;
  const controls = viewer.scene.screenSpaceCameraController;
  controls.minimumZoomDistance = MINIMUM_HEIGHT;
  controls.rotateEventTypes = LEFT_DRAG;
  controls.zoomEventTypes = [RIGHT_DRAG, PINCH];

  // Scale each wheel step to the current distance from the point under the cursor.
  const cursor = new Cesium.Cartesian2();
  const direction = new Cesium.Cartesian3();
  const hit = new Cesium.Cartesian3();
  viewer.canvas.addEventListener("wheel", event => {
    event.preventDefault();
    const rect = viewer.canvas.getBoundingClientRect();
    cursor.x = event.clientX - rect.left;
    cursor.y = event.clientY - rect.top;
    const camera = viewer.camera;
    const height = camera.positionCartographic.height;
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 120 : 1;
    const fraction = 1 - Math.exp(Cesium.Math.clamp(event.deltaY * unit / 420, -0.45, 0.45));
    const targetHeight = Cesium.Math.clamp(height * (1 - fraction), MINIMUM_HEIGHT, 45000000);
    const amount = (height - targetHeight) / height;
    if (camera.pickEllipsoid(cursor, viewer.scene.globe.ellipsoid, hit)) {
      Cesium.Cartesian3.subtract(hit, camera.positionWC, direction);
      const distance = Cesium.Cartesian3.magnitude(direction);
      Cesium.Cartesian3.normalize(direction, direction);
      camera.move(direction, distance * amount);
    } else {
      camera.zoomIn(height - targetHeight);
    }
    viewer.scene.requestRender();
  }, {passive: false});

  const base = riverImagery("blue-marble", layers["blue-marble"]);
  base.errorEvent.addEventListener(() => {
    status.hidden = false;
    status.textContent = "A Blue Marble tile failed to load from River.";
  });
  viewer.imageryLayers.addImageryProvider(base);

  const detail = layers["israel-2015"] ? "israel-2015" : "israel-2015-detail";
  const regions = ["israel-2015-overview", detail];
  const regionalLayers = regions.filter(id => layers[id]).map(id => ({
    id,
    overview: id === "israel-2015-overview",
    imagery: undefined,
  }));

  function updateRegionalLayers() {
    const height = viewer.camera.positionCartographic.height;
    for (const layer of regionalLayers) {
      const threshold = layer.overview
        ? OVERVIEW_HEIGHT
        : layer.imagery ? DETAIL_EXIT_HEIGHT : DETAIL_ENTER_HEIGHT;
      const visible = height < threshold;
      if (visible === Boolean(layer.imagery)) continue;
      if (visible) {
        const provider = riverImagery(layer.id, layers[layer.id]);
        provider.errorEvent.addEventListener(() => {
          status.hidden = false;
          status.textContent = "An Israel imagery tile failed to load from River.";
        });
        layer.imagery = new Cesium.ImageryLayer(provider, {
          magnificationFilter: layer.overview
            ? Cesium.TextureMagnificationFilter.LINEAR
            : Cesium.TextureMagnificationFilter.NEAREST,
        });
        viewer.imageryLayers.add(layer.imagery);
      } else {
        viewer.imageryLayers.remove(layer.imagery, true);
        layer.imagery = undefined;
      }
      viewer.scene.requestRender();
    }
  }

  viewer.scene.preUpdate.addEventListener(updateRegionalLayers);
  const destination = Cesium.Cartesian3.fromDegrees(34.7818, 32.0853, 22000000);
  const ground = Cesium.Cartesian3.fromDegrees(34.7818, 32.0853);
  Cesium.Cartesian3.subtract(ground, destination, direction);
  Cesium.Cartesian3.normalize(direction, direction);
  const right = Cesium.Cartesian3.cross(direction, Cesium.Cartesian3.UNIT_Z, new Cesium.Cartesian3());
  const up = Cesium.Cartesian3.cross(right, direction, new Cesium.Cartesian3());
  Cesium.Cartesian3.normalize(up, up);
  viewer.camera.setView({destination, orientation: {direction, up}});
  status.hidden = true;
}

start().catch(error => {
  status.hidden = false;
  status.textContent = error.message;
  console.error(error);
});

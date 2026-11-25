"""Matched WMTS navigation on an immutable, delayed range origin. Run from the River root."""
import argparse
import concurrent.futures
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

from PIL import Image

parser = argparse.ArgumentParser(description=__doc__)
for name in ("before", "after", "output"):
    parser.add_argument("--" + name, type=Path, required=True)
parser.add_argument("--intermediate", type=Path, help="also measure the pre-fix release in each matched case")
parser.add_argument("--grok", type=Path, help="directory containing lib/ and bin/")
parser.add_argument("--before-grok", type=Path, help="Grok install for the baseline binary when comparing codec libraries")
parser.add_argument("--variants", type=Path, help="directory with mixed-reference.png and mixed-single-{rpcl,lrcp}.jp2")
parser.add_argument("--repeats", type=int, default=3)
parser.add_argument("--port", type=int, default=19784)
parser.add_argument("--profile-zoom", action="store_true",
                    help="record sequential parent and child codec timings for plot-wmts-driver.py")
parser.add_argument("--driver-navigation", action="store_true", help="compare both native drivers on original preview, zoom and neighboring tiles")
parser.add_argument("--preview-comparison", action="store_true", help="measure preview, first full tile, horizontal sibling and repeat with origin range traces")
parser.add_argument("--viewport-path", action="store_true", help="overview, off-center zoom, horizontal then vertical pan, and backtrack")
parser.add_argument("--neighbor-clients", type=int, default=1,
                    help="number of simultaneous clients requesting the same immediate sibling")
parser.add_argument("--immediate-navigation", action="store_true",
                    help="send preview, full and sibling back-to-back, deferring pixel checks and origin polling")
parser.add_argument("--origin-delay-ms", type=float, default=4)
parser.add_argument("--origin-bytes-per-second", type=int, default=536870912)
args = parser.parse_args()
if args.viewport_path:
    args.preview_comparison = args.immediate_navigation = True
if args.preview_comparison:
    args.driver_navigation = True
base = f"http://127.0.0.1:{args.port}"
river = f"http://127.0.0.1:{args.port+1}"
cases = []
rows = []
expected = {}

def digest(data):
    return hashlib.sha256(data).hexdigest()

def get(url):
    with urllib.request.urlopen(url, timeout=120) as response:
        return response.read(), dict(response.headers)

def fetch_json(url):
    return json.loads(get(url)[0])

def ready(process, url):
    for _ in range(200):
        if process.poll() is not None:
            raise RuntimeError(f"process exited with {process.returncode}")
        try:
            get(url)
            return
        except OSError:
            time.sleep(.025)
    raise TimeoutError(url)

for f in json.loads(Path("target/fixtures/corpus.json").read_text())["fixtures"]:
    if f["file"].endswith(("d6-single.jp2", "d6-layers.jp2")):
        path = Path("corpus", f["file"]).resolve()
        assert digest(path.read_bytes()) == f["sha256"]
        cases.append((path, Path("corpus", f["source"]), "single" if "single" in path.name else "layer"))
if args.variants:
    for order in ("rpcl", "lrcp"):
        cases.append((args.variants / f"mixed-single-{order}.jp2", args.variants / "mixed-reference.png", "variant"))

def run(binary, build, trial, case, flow, driver, log):
    path, reference, kind = case
    scenario = f"{build}-{trial}-{path.name}-{flow}-{driver}"
    env = dict(os.environ, RIVER_BIND=f"127.0.0.1:{args.port+1}", RIVER_CONFIG_MODE="env",
               RIVER_OTEL_ENABLED="false", RIVER_ENABLED_DRIVERS="openjpeg,jp2grok",
               RIVER_AUTO_ADAPTATION_MODE="liberal,no-disk", RIVER_PROFILE="auto",
               RIVER_S3_ADAPTIVE_READS="adaptive", RIVER_S3_MAX_OVERFETCH_RATIO="2",
               RIVER_S3_RANGE_OVERFETCH_RATIO="2", RIVER_S3_SOURCE_COUNT="1")
    source = dict(ID="fixture", NAME="fixture", BUCKET="fixture", REGION="us-east-1", ACCESS_KEY_ID="fixture",
                  SECRET_ACCESS_KEY="fixture", ALLOW_HTTP="true", VIRTUAL_HOSTED_STYLE="false", ENDPOINT=f"{base}/{scenario}")
    env.update({"RIVER_S3_SOURCE_1_"+key: value for key, value in source.items()})
    for key in "MULTIPART_PLT MULTIPART_PREFIX SINGLE_PREFIX INDEX_ONLY_PREFIX STREAMED_PLT_ROI PRECINCT_ROI EXACT_SUPPORT".split():
        env["RIVER_S3_"+key] = "true"
    grok_root = (args.before_grok if build == "before" else None) or args.grok
    if grok_root:
        lib = grok_root / "lib"
        env.update(RIVER_GROK_NATIVE="true", RIVER_GROK_NATIVE_LIBRARY=str(lib/"libriver_grok.so"),
                   RIVER_GROK_DECOMPRESS=str(grok_root/"bin/grk_decompress"), LD_LIBRARY_PATH=str(lib), RIVER_GROK_NATIVE_THREADS="8")
    if args.profile_zoom or args.driver_navigation:
        env.update(RIVER_PROFILE_CODEC="1", RIVER_PROFILE_SOURCE="1", RIVER_LOG_LEVEL="debug",
                   RIVER_LOG_VERBOSE="true", RIVER_LOG_REQUEST_SUMMARY="true")
        print(f"PROFILE {build} {trial} {path.name}", file=log, flush=True)
    server = subprocess.Popen([binary.resolve()], env=env, stdout=log, stderr=log)
    try:
        ready(server, river+"/health")
        locator = "s3-" + json.dumps(["fixture", path.name], separators=(",", ":")).encode().hex()
        manifest = fetch_json(river+"/api/assets/"+locator+"/manifest")
        asset = manifest["assetId"]
        def request(tile):
            red, col, row, layers = tile
            query = dict(SERVICE="WMTS", REQUEST="GetTile", VERSION="1.0.0", LAYER=asset, STYLE="default",
                         FORMAT="image/png", DRIVER=driver, TILEMATRIXSET="RiverPixel-"+asset,
                         TILEMATRIX=manifest["resolutionLevels"]-1-red, TILEROW=row, TILECOL=col, LAYERS=layers)
            return get(river+"/wmts?"+urllib.parse.urlencode(query))
        steps = []
        if flow in ("preview", "pause", "fanout"):
            steps.append(("preview", [(3 if kind=="single" else 0,0,0,1)]))
        if flow=="zoom":
            steps.append(("parent", [(1,1,1,0)]))
        x,y = (2,2) if flow in ("zoom","cold") else (0,0)
        if kind=="variant":
            steps.append(("parent", [(1,1,1,0)]))
            x,y=6,7
        if flow=="fanout":
            steps.append(("concurrent", [(0,0,0,0),(0,1,0,0),(0,2,0,0),(0,0,2,0)]))
        else:
            steps += [(phase,[(0,col,row,0)]) for phase,col,row in
                      [("firstFull",x,y),("sibling",x+1,y),("vertical",x,y+1 if kind!="variant" else y-1),
                       ("distant",0,7),("repeat",x,y)]]
        if args.preview_comparison:
            steps = [step for step in steps if step[0] in ("preview","firstFull","sibling","repeat")]
        if args.viewport_path:
            steps = [(phase, [tile]) for phase,tile in [
                ("preview",(3,0,0,1)), ("parent",(1,1,1,0)), ("firstFull",(0,3,3,0)),
                ("east1",(0,4,3,0)), ("east2",(0,5,3,0)), ("east3",(0,6,3,0)),
                ("south1",(0,6,4,0)), ("south2",(0,6,5,0)), ("south3",(0,6,6,0)),
                ("backtrack",(0,6,5,0))]]
        if args.profile_zoom:
            steps = steps[:2]
        initial = fetch_json(base+"/__stats/"+scenario)
        sequence_wire = initial["wire"]
        immediate = {}
        if args.immediate_navigation:
            assert args.preview_comparison
            for phase, tiles in steps:
                if phase == "sibling":
                    tiles[:] = tiles * args.neighbor_clients
                start = time.monotonic()
                if len(tiles) == 1:
                    responses = [request(tiles[0])]
                else:
                    with concurrent.futures.ThreadPoolExecutor(len(tiles)) as pool:
                        responses = list(pool.map(request, tiles))
                end = time.monotonic()
                immediate[phase] = (responses, start, end)
            final = fetch_json(base+"/__stats/"+scenario)
            def at(moment):
                events = [e for e in final["ranges"] if e.get("end", float("inf")) <= moment]
                bodies = [e for e in events if e["body"]]
                def length(e):
                    lo, hi = map(int, e["range"].removeprefix("bytes=").split("-"))
                    return hi - lo + 1
                return dict(ranges=events, pressure=final["pressure"],
                            wire=dict(gets=len(bodies), bytes=sum(map(length, bodies))))
            sequence_wire = at(immediate["preview"][1])["wire"]
        with Image.open(reference) as image:
            image.load()
            for phase, tiles in steps:
                if phase=="firstFull" and flow=="pause":
                    time.sleep(.08)
                if args.immediate_navigation:
                    responses, start, end = immediate[phase]
                    ms = (end - start) * 1000
                    snapshot, observation = at(start), at(end)
                else:
                    snapshot = fetch_json(base+"/__stats/"+scenario)
                    before = snapshot["wire"]
                    start = time.monotonic()
                    if len(tiles)==1:
                        responses = [request(tiles[0])]
                    else:
                        with concurrent.futures.ThreadPoolExecutor(len(tiles)) as pool:
                            responses = list(pool.map(request, tiles))
                    ms = (time.monotonic()-start)*1000
                    observation=fetch_json(base+"/__stats/"+scenario)
                before = snapshot["wire"]
                after=observation["wire"]
                hashes = []
                for tile,(data,headers) in zip(tiles,responses):
                    red,col,row,layers=tile
                    with Image.open(io.BytesIO(data)) as downloaded:
                        pixels=downloaded.convert("RGB")
                        sha=digest(pixels.tobytes())
                        if red==0 and layers==0:
                            crop=image.crop((512*col,512*row,min(512*(col+1),image.width),min(512*(row+1),image.height))).convert("RGB")
                            assert pixels.size==crop.size and sha==digest(crop.tobytes()), (scenario,phase,tile)
                        key=(path.name,driver,tile)
                        assert sha==expected.setdefault(key,sha), (scenario,tile,"baseline mismatch")
                        hashes.append(sha)
                rows.append(dict(build=build,trial=trial,image=path.name,flow=flow,driver=driver,phase=phase,
                                 requested_tiles=tiles,ms=ms,gets=after["gets"]-before["gets"],bytes=after["bytes"]-before["bytes"],
                                 sequence_gets=after["gets"]-sequence_wire["gets"], sequence_bytes=after["bytes"]-sequence_wire["bytes"],
                                 peak=observation["pressure"]["peak"],backends=[h.get("x-river-decoder-backend") for _,h in responses],
                                 prep_ms=[float(h["x-river-source-cache-ms"]) for _,h in responses],
                                 decode_ms=[float(h["x-river-decode-ms"]) for _,h in responses],
                                 routes=[h.get("x-river-decoder-route") for _,h in responses],hashes=hashes))
                if args.immediate_navigation:
                    rows[-1]["elapsed_from_preview_ms"] = (end - immediate["preview"][1]) * 1000
                    rows[-1]["shared_wait_ms"] = [
                        float(dict(segment.strip().split(";dur=") for segment in h["server-timing"].split(",")).get("shared_wait", 0))
                        for _, h in responses]
                if args.preview_comparison:
                    if phase == "firstFull":
                        rows[-1]["prior_ranges"] = snapshot["ranges"]
                    rows[-1]["origin_ranges"] = observation["ranges"][len(snapshot["ranges"]):]
                if args.driver_navigation:
                    headers = responses[0][1]
                    rows[-1]["grok_ms"] = {key: float(headers["x-river-grok-"+key+"-ms"])
                        for key in ("native", "decompress", "conversion") if "x-river-grok-"+key+"-ms" in headers}
                    for key in ("hits", "misses"):
                        rows[-1]["block_"+key] = int(headers.get("x-river-block-"+key, 0))
                if args.profile_zoom or args.driver_navigation:
                    timing = responses[0][1]["server-timing"]
                    rows[-1]["stages"] = dict(stage.strip().split(";dur=") for stage in timing.split(","))
        print(scenario,flush=True)
    finally:
        server.terminate()
        server.wait()

with tempfile.TemporaryDirectory() as temp, (args.output.with_suffix(".log")).open("w") as log:
    manifest=Path(temp,"manifest.json")
    manifest.write_text(json.dumps(dict(cases=[dict(file=str(path.resolve()),name=path.name,sha256=digest(path.read_bytes())) for path,_,_ in cases])))
    origin=subprocess.Popen([sys.executable,"tools/benchmarks/serve-fixed-native-origin.py","--manifest",manifest,
                             "--port",str(args.port),"--tcp-nodelay","--delay-ms",str(args.origin_delay_ms),"--body-bytes-per-second",str(args.origin_bytes_per_second)],stdout=log,stderr=log)
    try:
        ready(origin,base+"/__manifest")
        builds=[("before",args.before),("after",args.after)]
        if args.intermediate:
            builds.insert(1, ("intermediate", args.intermediate))
        for trial in range(args.repeats):
            if args.intermediate:
                start = trial % len(builds)
                trial_builds = builds[start:] + builds[:start]
            else:
                trial_builds = builds[::1 if trial % 2 == 0 else -1]
            for case in cases:
                flows={"single":["preview","zoom","cold","fanout"],"layer":["preview","pause"],"variant":["edge"]}[case[2]]
                if args.profile_zoom:
                    flows = ["zoom"] if case[2] == "single" else []
                if args.driver_navigation:
                    flows = ["preview", "zoom"] if case[2] == "single" else ["preview"] if case[2] == "layer" else []
                if args.preview_comparison:
                    flows = ["preview"] if case[2] in ("single", "layer") else []
                for flow in flows:
                    drivers=["openjpeg","jp2grok"] if args.grok and flow in ("edge","preview") and case[2]!="layer" else ["openjpeg"]
                    if args.driver_navigation and args.grok:
                        drivers = ["openjpeg", "jp2grok"]
                    for driver in drivers:
                        for build,binary in trial_builds:
                            run(binary,build,trial,case,flow,driver,log)
                            args.output.write_text(json.dumps(rows))
    finally:
        origin.terminate()
        origin.wait()

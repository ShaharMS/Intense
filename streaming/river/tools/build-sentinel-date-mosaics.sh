#!/usr/bin/env bash
set -euo pipefail

input_root=/input
output_root=/output
work_root=/tmp/river-date-mosaics
max_dates="${MAX_DATES:-20}"

if [[ ! -f "$input_root/atlas.json" ]]; then
  echo "Missing $input_root/atlas.json; run download-sentinel-atlas.ps1 first." >&2
  exit 1
fi
if ! [[ "$max_dates" =~ ^[1-9][0-9]*$ ]]; then
  echo "MAX_DATES must be a positive integer, got: $max_dates" >&2
  exit 1
fi

mkdir -p "$output_root" "$work_root"

# The selection and every source object are retained in the output manifest
# for reproducibility. MAX_DATES=35 selects every acquisition in this atlas.
python3 - "$input_root/atlas.json" "$input_root" "$output_root/mosaic-manifest.json" "$max_dates" > "$work_root/dates.txt" <<'PY'
import json
import pathlib
import sys

atlas_path = pathlib.Path(sys.argv[1])
input_root = pathlib.Path(sys.argv[2])
output_path = pathlib.Path(sys.argv[3])
requested_count = int(sys.argv[4])
atlas = json.loads(atlas_path.read_text(encoding="utf-8-sig"))
scenes = atlas["scenes"]

if requested_count >= len(scenes):
    selected = scenes
else:
    # Keep a representative temporal spread for an explicitly smaller run.
    selected = [scenes[round(index * (len(scenes) - 1) / (requested_count - 1))]
                for index in range(requested_count)] if requested_count > 1 else [scenes[0]]

missing = []
for scene in selected:
    date = scene["acquired"]
    for item in scene["objects"]:
        path = input_root / date / item["square"] / f'{item["band"]}.jp2'
        if not path.is_file() or path.stat().st_size != item["bytes"]:
            missing.append(str(path))
if missing:
    raise SystemExit("Atlas is incomplete or corrupt:\n" + "\n".join(missing))

manifest = {
    "title": "River Sentinel-2 per-date RGB JP2 mosaics",
    "sourceAtlas": str(atlas_path.name),
    "outputFormat": "JP2OpenJPEG lossless RGB, 1024 x 1024 tiles, 7 resolutions",
    "selectedSceneCount": len(selected),
    "scenes": selected,
}
output_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
for scene in selected:
    print(scene["acquired"])
PY

mapfile -t squares < <(python3 - "$input_root/atlas.json" <<'PY'
import json
import sys
print("\n".join(json.load(open(sys.argv[1], encoding="utf-8-sig"))["squares"]))
PY
)

while IFS= read -r date; do
  target="$output_root/$date.jp2"
  if [[ -s "$target" ]] && gdalinfo -json "$target" >/dev/null; then
    echo "already complete: $target"
    continue
  fi

  date_work="$work_root/$date"
  mkdir -p "$date_work"
  echo "building $target"
  for band in B04 B03 B02; do
    sources=()
    for square in "${squares[@]}"; do
      sources+=("$input_root/$date/$square/$band.jp2")
    done
    gdalbuildvrt -q -strict -resolution highest -overwrite "$date_work/$band.vrt" "${sources[@]}"
  done

  gdalbuildvrt -q -separate -overwrite "$date_work/rgb.vrt" \
    "$date_work/B04.vrt" "$date_work/B03.vrt" "$date_work/B02.vrt"
  # Only remove this generated, incomplete temp file; completed `.jp2` files
  # above are validated and retained for a resumable run.
  rm -f "$target.partial.jp2"
  gdal_translate -q -of JP2OpenJPEG \
    -colorinterp red,green,blue \
    -co REVERSIBLE=YES -co QUALITY=100 -co BLOCKXSIZE=1024 -co BLOCKYSIZE=1024 \
    -co RESOLUTIONS=7 -co PROGRESSION=RPCL -co YCC=NO -co GEOJP2=YES -co GMLJP2=YES \
    "$date_work/rgb.vrt" "$target.partial.jp2"
  gdalinfo -json "$target.partial.jp2" > "$target.info.json"
  mv -f "$target.partial.jp2" "$target"
done < "$work_root/dates.txt"

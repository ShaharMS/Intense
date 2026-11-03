#!/usr/bin/env bash
set -euo pipefail

# Ingestion-only JPEG 2000 pyramid builder. The 1:16 level already exists;
# generate a 1:32 gain level from it so serving pods never transcode requests.
root="${1:-/output}"
input="$root/river-preview-jp2"
output="$root/river-gain-jp2"
mkdir -p "$output"

for source in "$input"/????-??-??.jp2; do
  [ -f "$source" ] || continue
  target="$output/$(basename "$source")"
  if [ -s "$target" ] && gdalinfo -json "$target" >/dev/null 2>&1; then
    echo "already valid: $target"
    continue
  fi
  partial="$target.partial.jp2"
  rm -f "$partial"
  gdal_translate -of JP2OpenJPEG \
    -outsize 50% 50% \
    -co REVERSIBLE=YES \
    -co BLOCKXSIZE=1024 \
    -co BLOCKYSIZE=1024 \
    -co RESOLUTIONS=6 \
    "$source" "$partial"
  gdalinfo -json "$partial" >/dev/null
  mv -f "$partial" "$target"
  echo "published: $target"
done

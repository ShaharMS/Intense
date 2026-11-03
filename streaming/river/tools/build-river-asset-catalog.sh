#!/usr/bin/env bash
set -euo pipefail

# Ingestion-only: creates immutable IDs before serving pods start. River reads
# this small catalog and never walks or hashes the JP2 object corpus itself.
root="${1:-/output}"
catalog="$root/river-assets.json"
tmp="$catalog.partial"

python3 - "$root" "$tmp" <<'PY'
import hashlib, json, pathlib, sys
root, target = map(pathlib.Path, sys.argv[1:])
assets = []
for source in sorted(root.glob('????-??-??.jp2')):
    info = json.loads((pathlib.Path(str(source) + '.info.json')).read_text())
    width, height = info['size']
    block = info.get('bands', [{}])[0].get('block', [])
    digest = hashlib.sha256()
    with source.open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            digest.update(chunk)
    preview = pathlib.Path('river-preview-jp2') / source.name
    assets.append({
        'assetId': digest.hexdigest(), 'width': width, 'height': height,
        'bitDepth': 16, 'source': source.name,
        'nativeTileWidth': block[0] if len(block) == 2 else None,
        'nativeTileHeight': block[1] if len(block) == 2 else None,
        'previewSource': str(preview) if (root / preview).is_file() else None,
        'previewScale': 16,
    })
target.write_text(json.dumps({'version': 1, 'assets': assets}, indent=2) + '\n')
PY
mv -f "$tmp" "$catalog"
echo "published $catalog"

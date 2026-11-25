"""Generate repeatable JPEG 2000 fixtures locally; no imagery downloads.

Only images go in corpus. Encoding options, checksums, and exact pixel checks
go in target/fixtures. Pillow with OpenJPEG support is required.
"""
import argparse
import hashlib
import json
import random
import struct
import time
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, features

ROOT = Path(__file__).resolve().parents[2]


def add_tlm(data):
    box = 0
    while data[box + 4:box + 8] != b"jp2c":
        size = struct.unpack_from(">I", data, box)[0]
        if size < 8:
            raise ValueError("Invalid JP2 box")
        box += size
    size = struct.unpack_from(">I", data, box)[0]
    if size != len(data) - box:
        raise ValueError("Expected final, explicitly sized codestream box")
    at = box + 10
    while data[at:at + 2] != b"\xff\x90":
        marker, length = struct.unpack_from(">HH", data, at)
        if marker == 0xff55 or length < 2:
            raise ValueError("Unexpected main header")
        at += length + 2
    first = at
    entries = []
    while data[at:at + 2] == b"\xff\x90":
        tile, length = struct.unpack_from(">HI", data, at + 4)
        if length < 14:
            raise ValueError("Unbounded tile part")
        entries.append((tile, length))
        at += length
    if data[at:] != b"\xff\xd9":
        raise ValueError("Invalid tile-part chain")
    tlm = bytearray()
    for z, start in enumerate(range(0, len(entries), 10921)):
        part = entries[start:start + 10921]
        tlm.extend(struct.pack(">HHBB", 0xff55, 4 + 6 * len(part), z, 0x60))
        for tile, length in part:
            tlm.extend(struct.pack(">HI", tile, length))
    prefix = bytearray(data[:first])
    struct.pack_into(">I", prefix, box, size + len(tlm))
    return prefix + tlm + data[first:]


def source_image(kind, size, seed):
    rng = random.Random(seed)
    noise = Image.frombytes("RGB", (size, size), rng.randbytes(size * size * 3))
    if kind == "texture":
        return noise
    base = Image.new("RGB", (size, size))
    draw = ImageDraw.Draw(base)
    if kind == "terrain":
        for y in range(size):
            draw.line((0, y, size, y), fill=(y * 211 // size, 90 + y * 100 // size, 180 - y * 120 // size))
        for _ in range(350):
            x, y = rng.randrange(size), rng.randrange(size)
            r = rng.randrange(8, max(9, size // 8))
            draw.ellipse((x-r, y-r, x+r, y+r), fill=(rng.randrange(256), rng.randrange(256), rng.randrange(256)))
    else:
        step = max(8, size // 64)
        for y in range(0, size, step):
            for x in range(0, size, step):
                c = rng.randrange(40, 240)
                draw.rectangle((x+2, y+2, x+step-3, y+step-3), fill=(c, c, c))
    return ImageChops.add(base, noise, scale=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--replace-generated", action="store_true", help="Replace files created by an earlier generator run")
    parser.add_argument("--size", type=int, default=4096)
    parser.add_argument("--output", type=Path, default=ROOT / "corpus")
    parser.add_argument("--manifest", type=Path, default=ROOT / "target/fixtures/corpus.json")
    args = parser.parse_args()
    if not features.check("jpg_2000"):
        parser.error("Pillow needs OpenJPEG encoding support")
    if not 128 <= args.size <= 16384:
        parser.error("size must be 128 through 16384")
    if args.output.exists() and any(args.output.glob("synthetic-*")) and not args.replace_generated:
        parser.error("Generated files already exist; choose a different output or explicitly use --replace-generated")
    args.output.mkdir(parents=True, exist_ok=True)
    args.manifest.parent.mkdir(parents=True, exist_ok=True)
    layouts = [
        ("plain-d4", 4, "LRCP", False, False, 512, [1]),
        ("tlm-d4", 4, "RPCL", True, False, 512, [1]),
        ("tlm-plt-d5", 5, "RPCL", True, True, 512, [1]),
        ("tlm-plt-d6-layers", 6, "RPCL", True, True, 1024, [16, 8, 4, 2, 1]),
        ("tlm-plt-d6-single", 6, "RLCP", True, True, args.size, [1]),
    ]
    records = []
    for seed, kind in enumerate(("terrain", "urban", "texture"), 1227):
        source = source_image(kind, args.size, seed)
        reference = hashlib.sha256(source.tobytes()).hexdigest()
        source_path = args.output / f"synthetic-{kind}-reference.png"
        source.save(source_path, compress_level=1)
        for name, levels, progression, tlm, plt, tile, layers in layouts:
            path = args.output / f"synthetic-{kind}-{name}.jp2"
            start = time.monotonic()
            source.save(path, format="JPEG2000", irreversible=False,
                        num_resolutions=levels+1, tile_size=(tile, tile),
                        progression=progression, quality_mode="rates",
                        quality_layers=layers, plt=plt, mct=1)
            if tlm:
                path.write_bytes(add_tlm(path.read_bytes()))
            with Image.open(path) as decoded:
                actual = hashlib.sha256(decoded.convert("RGB").tobytes()).hexdigest()
            if actual != reference:
                raise RuntimeError(f"Pixel mismatch: {path}")
            record = dict(file=path.name, source=source_path.name, synthetic=True,
                          bytes=path.stat().st_size, sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                          width=args.size, height=args.size, components=3, bitDepth=8,
                          waveletLevels=levels, progression=progression, tlm=tlm, plt=plt,
                          nativeTileSize=tile, qualityLayers=layers, referencePixelsSha256=reference,
                          pixelsExact=True, seconds=time.monotonic()-start)
            records.append(record)
            args.manifest.write_text(json.dumps(dict(complete=len(records)==15, fixtures=records), indent=2)+"\n")
            print(json.dumps(record), flush=True)


if __name__ == "__main__":
    main()

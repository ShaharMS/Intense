This is generated diagnostic imagery, not a production source or a runtime asset.
It is compiled into tests only and is excluded from the deployed binary.

The 512 x 512 unsigned grayscale input is generated as
`(x*13 + y*7 + (x*y)%251)%256` for every pixel. OpenJPEG 1.5.2 encodes it with:

`-n 4 -t 64,64 -r 8,4,0 -p RPCL -TP R -jpip`

The isolated encoder has only a modern-MSVC compatibility guard change: its old
`lrintf` replacement is disabled for MSVC >= 1800. River does not link this encoder.
Modern Pillow/OpenJPEG independently decoded every pixel exactly. The fixture has
64 native tiles, four resolution tile-parts each, and a real Part9 cidx/tpix/faix
index. Every one of its 256 indexed extents matches the actual SOT header.

`provenance.json` records the source-pixel and JP2 hashes. Acquisition, build and
independent verification evidence is under `reports/kakadu-io-m303/openjpeg15-reference-m378/`.

`unindexed-quality-split.jp2` is a negative prefix-coverage diagnostic, encoded
from that same input with `-r 20,10,0 -n 4 -t 64,64 -p RPCL -TP L`.
Its 768 encoded tile-parts are reordered by ordinal then tile, preserving each
tile's part order; optional indexes are omitted. A complete first-part run does
not prove complete quality coverage. The unindexed coarse-prefix test must reject
this candidate for both first-layer and full-quality requests. Independent
Pillow/OpenJPEG reduction-3 pixels match the original lossless reference.
SHA-256: `541ed4c1fc3ef5944739626b8b4a10fd00fe1a2c462a901411ab91cbae134c39`.
Generation and proof: `reports/kakadu-io-m303/verify-quality-split-m401.py`
and `quality-split-proof-m401.json`.

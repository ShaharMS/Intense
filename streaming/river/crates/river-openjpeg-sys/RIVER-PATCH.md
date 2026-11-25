# River decoder allocation patch

Vendored from the Cargo registry's `openjpeg-sys` 1.0.12 (OpenJPEG 2.5.3).
The upstream BSD license is in `vendor/LICENSE`. Bindings, public ABI and
encoder behavior are unchanged.

River defers decoder tag trees and codeblock segment arrays until the first
nonempty packet, and codeblock storage until a precinct has packet data.
Precinct geometry remains initialized before the decoder's ROI selection.
Tile reuse resizes existing tag trees before reading the first layer.
Codeblock reuse clears decoded buffers and packet state even when its segment
array has never been allocated; `empty_block_reuse.c` checks that invariant.
Entropy bytes, wavelet transforms, quality layers and output precision are
unchanged. The shared Cargo registry is never modified. The build script tracks
vendored C files, headers, and configuration so decoder edits invalidate Cargo's
compiled native library cache.

The isolated patch and source provenance are recorded under
`reports/deep-aerial-m293/openjpeg-sparse-v2.patch`. Initial independent pixel
checks cover 128 classic coding-style/edge cases and 10 large aerial ROIs.
Server and release qualification must be recorded separately.

The later deferred-precinct-geometry experiments are deliberately excluded:
their edge-case correctness checks failed.

The Tier-1 MQ decoder shares the conditional symbol exchange for both
subintervals (ISO/IEC 15444-1 Annex C). The selected subinterval determines
the interval update and whether the probability comparison is inverted.
Renormalization, byte refill, probability tables and the encoder are unchanged.
The standalone differential test `tests/mq_decision.c` checks the decoded
symbol, next state, interval, code register and input cursor across every
normalized interval and probability state, on both sides of Qe. Build it
against the vendored static library with the same config and include paths.

River can supply a request-scoped Tier-1 coefficient callback. The submitting
thread passes the session to each worker. Only a nonempty, successfully decoded
block is stored, before ROI adjustment and dequantization. Hits restore a private
copy before any in-place transform. The host checks the original object revision,
complete quality, component, subband, resolution, block bounds, bit planes,
passes and compressed length. The unregistered codec follows its normal path.

The sparse S3 reader can call `river_opj_t1_word` as soon as it has a
complete reversible Part 1 MQ codeword. River derives its bit-plane count
from QCD and the packet's zero-bit-plane tag tree. The native entry point
uses the same Tier-1 routine as ordinary tile decode, with a worker-private
scratch buffer for the MQ terminator. Its returned coefficient buffer belongs
to that Tier-1 handle until the next decode or destruction. River publishes
an owned copy under the source revision, block geometry and driver
representation. Mercury uses sign-magnitude coefficients aligned to the
subband precision. OpenJPEG uses its native signed coefficients. The complete
compressed body remains available to the ordinary decoder on a cache miss.


For full-quality partial windows of 16,384 to 1,048,576 reference pixels
with two to four components, River schedules the independent component
inverse wavelet transforms on the decoder's existing worker pool. Each
transform owns its sparse workspace. The caller joins all jobs before color
conversion or error return. Quality-limited previews use serial component
reconstruction so their decode can overlap source preparation for refinement.

For multilayer packet-range views, River can register an in-memory Tier-1
continuation session on the OpenJPEG submitting thread. Default-style,
single-segment Part 1 MQ blocks can save coefficients, neighborhood flags,
MQ registers and probability context indices at a coding-pass boundary.
The preview saves only before the decoder consumes synthetic termination
bytes. Restoration checks the source revision, block identity, input prefix
and pass count. The codec reconstructs cursors relative to the new segment.
Once the block finishes, exact segment bytes and an identical pass count
allow adjacent windows to reuse its final coefficients, including blocks
whose final passes consumed synthetic termination bytes. Unsupported coding
styles, missing states and cache collisions use normal Tier-1 decoding.

For eligible multilayer PLT packet-range views, River also assembles complete
default-style reversible MQ segments as cached blocks and S3 range groups
arrive. It retains inclusion, zero-plane and length state across quality
layers, checks the PLT extent against the parsed header and bodies, and
decodes available segments on spare workers. They publish through the same
revision-scoped finished-coefficient cache. Native decode checks the exact
segment bytes, geometry and pass count before accepting an early result.
The arrival observer issues no reads. Preparation ends speculative work so
the native decoder can acquire its normal CPU budget.

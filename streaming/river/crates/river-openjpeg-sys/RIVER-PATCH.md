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

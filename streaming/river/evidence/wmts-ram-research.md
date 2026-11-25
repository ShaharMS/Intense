# WMTS research directions

The serving pod uses RAM only. Source images normally stay fixed and can occasionally be replaced manually. Scope shared state to a source revision. Derived tile storage outside the pod is out of scope.

## Retained proposals

- Share in-flight reads of overlapping blocks and dispatch disjoint misses independently.
- Start refinement for the preview footprint while the preview finishes. Let a foreground full-quality request join and promote that work.
- Retain parsed codeblock coordinates and compressed spans per source revision in RAM.
- Apply the same index to intermediate zooms and reuse compressed spans across parent and child views.
- Price gap merging by completion time, aggregate bandwidth, pending reads and subsequent consumption of extra bytes.

## Solutions for the four scheduling restrictions

1. **Fixed batch and per-source GET ceilings.** Enqueue every known required miss. Let all tiles drawing from an endpoint share available transport capacity, with foreground demand ahead of speculative refinement. Account for both active operations and bytes in flight across the pod. Grow the dispatch window while useful throughput improves and back off on queueing or throttling. Honor explicit operator limits and request deadlines. Update both the dispatcher and the planner's estimate of available lanes so that planning does not merge ranges to meet an obsolete cap.
2. **Batch-size and RTT cutoffs.** Replace the four-versus-sixteen lane decision with the same scheduler. Seed its window using aggregate bytes per second times unloaded RTT and the actual pending range sizes. Learn from completed work and latency under load. Reuse endpoint observations across cold images. A batch crossing 1 MiB or a latency estimate crossing 5 ms should not abruptly change its parallelism.
3. **Image-wide fetch lock.** Atomically claim missing aligned blocks by source revision, join any existing fetch, and dispatch the unclaimed spans immediately. Publish each successful block before waking waiters. On failure release the claim for retry. A canceled tile drops its interest while other waiters retain the fetch. Coalesce adjacent claims where beneficial and charge each wire attempt once.
4. **All-header barrier.** On the supported sparse reader path, parse a packet as soon as its header bytes arrive. Validate offsets and charge the cumulative byte budget before enqueueing its payload. Prioritize headers that unlock further reads. Retain packet ordinals for ordered codestream assembly. If a later packet requires fallback, release unused work and keep verified source bytes reusable. For an already indexed packet, issue its payload immediately.

Before implementation, the local navigation trace peaked at 16 active GETs. In start-time order its four groups contained 1, 67, 48 and 48 GETs, separating groups at an idle gap over 40 ms. The two 48-GET groups occupied 25.0 and 29.2 ms from the first origin start to the last origin completion. That span excludes River's work before and after the origin calls. It gives an upper bound on time recoverable solely by compressing each observed origin interval. The matched release comparison below measures the combined changes.

## Related scheduling question

- Release source-I/O admission when preparation finishes and bound the prepared bytes waiting for decode.

## Implemented scope

River now shares overlapping SDK ranges within a source revision. A waiting tile has its own deadline, and the last waiter leaving cancels an unfinished read. Completed reads stay available to joiners through publication to the block cache. Disjoint ranges can transfer concurrently. The specialized native serial transport and streamed metadata scanner keep their existing admission behavior.

The sparse reader retains parsed codeblock geometry, header fields and byte offsets on the remote image handle within its metadata budget. It can select intermediate resolutions on compatible single-layer reversible images. The broad whole-image preview keeps its contiguous prefix path and uses spare source-I/O capacity to warm the index. Explicit quality-layer previews can prepare a full tile while the preview decodes. Background full-quality decoding waits for preview decoding to finish and takes an available CPU slot. A foreground request joins the same result when refinement is in progress.

On a cold index River can start a required payload read before the remaining headers arrive, then plan the rest together. Dispatch uses endpoint observations, available slots and a pod-wide byte budget. The range planner evaluates how many round trips a merge can remove and credits gap bytes previously consumed from cache. Explicit request and byte budgets still apply. Prepared WMTS and viewport work can release source-I/O admission while it waits for CPU. When the prepared-byte allowance is occupied the existing I/O permit provides backpressure.

The release comparison uses the same original JP2s, OpenJPEG and native Grok, a local fixed range origin with 4 ms delay and 536870912 bytes/s shared across responses, adaptive reads, 2× overfetch, and liberal/no-disk mode. Each navigation sequence starts a fresh River process. The manifest is resolved before tile timing. Source and tile caches live within the pod. The benchmark harness writes reports locally outside River. Each full-resolution all-layer tile is checked against the original reference pixels. Preview and reduced-resolution pixels are compared across builds. See [benchmark harness](../tools/benchmarks/benchmark-wmts-ram.py) and [raw timings](../target/benchmarks/wmts-ram-release.json).

### Matched release results

Times are median client milliseconds. The original-image rows pool three trials for each of terrain, urban and texture. The alternate-encoding rows pool three trials each of RPCL and LRCP. Traffic columns are median origin observations during the timed phase. Work started during preview or the pause can appear earlier in the sequence.

| Layout and driver | Request | Before ms | After ms | Before GETs | After GETs | Before MB | After MB |
|---|---|---:|---:|---:|---:|---:|---:|
| Multilayer, OpenJPEG | preview / preview | 48.3 | 72.6 | 13 | 19 | 0.21 | 3.20 |
| Multilayer, OpenJPEG | preview / firstFull | 104.9 | 65.2 | 6 | 0 | 2.98 | 0.00 |
| Multilayer, OpenJPEG | pause / firstFull | 106.2 | 2.4 | 6 | 0 | 2.98 | 0.00 |
| Single layer, OpenJPEG | preview / preview | 63.4 | 64.3 | 1 | 10 | 0.59 | 0.67 |
| Single layer, OpenJPEG | preview / firstFull | 152.7 | 158.5 | 62 | 53 | 4.48 | 4.40 |
| Single layer, native Grok | preview / firstFull | 165.9 | 174.0 | 62 | 53 | 4.48 | 4.40 |
| Single layer, OpenJPEG | cold / firstFull | 198.6 | 218.5 | 70 | 71 | 7.26 | 7.27 |
| Single layer, OpenJPEG | zoom / parent | 244.3 | 208.8 | 12 | 65 | 11.54 | 3.36 |
| Single layer, OpenJPEG | zoom / firstFull | 150.9 | 219.4 | 51 | 51 | 2.77 | 3.03 |
| Single layer, OpenJPEG | preview / sibling | 2.0 | 2.1 | 0 | 0 | 0.00 | 0.00 |
| Single layer, OpenJPEG | preview / vertical | 143.1 | 187.4 | 48 | 38 | 2.69 | 3.61 |
| Single layer, OpenJPEG | preview / distant | 118.8 | 133.3 | 48 | 48 | 4.63 | 4.63 |
| Single layer, OpenJPEG | preview / repeat | 1.8 | 2.1 | 0 | 0 | 0.00 | 0.00 |
| Single layer, OpenJPEG | fanout / concurrent | 818.9 | 818.0 | 166 | 160 | 12.41 | 11.79 |
| Alternate encodings, OpenJPEG | edge / firstFull | 136.4 | 190.4 | 44 | 70 | 1.76 | 2.78 |
| Alternate encodings, native Grok | edge / firstFull | 104.8 | 159.9 | 46 | 70 | 1.67 | 2.78 |

| Original image | Single layer, first full after preview (ms) | Multilayer, first full after preview (ms) |
|---|---:|---:|
| Terrain | 152.7 → 158.5 | 101.7 → 62.8 |
| Urban | 146.9 → 158.1 | 106.9 → 63.5 |
| Texture | 159.4 → 163.4 | 104.9 → 68.5 |

Across 150 fresh-process sequences the harness verified 864 WMTS responses. The release SHA-256 is `5cc5a0eb612bd64523f59db80ba112fb85b23378a10819189460101805036f14`. Default and Grok-enabled server suites each passed 328 tests with 16 ignored. The core suite passed one test.

Peak simultaneous origin GETs across the matched runs rose from 16 to 37. For immediate multilayer refinement, the median paired sum of preview and full request durations fell from 153.2 to 136.0 ms. Both versions used a median 19 GETs and 3.2 MB through the full response. For the single-layer child after its parent, the paired sum rose from 395.2 to 423.0 ms. Median child preparation rose from 44.1 to 84.5 ms and child decode rose from 97.1 to 112.4 ms. These module timings describe where time went within each build. The combined benchmark cannot attribute the difference to any one change.

## Single-layer regression investigation

All three release binaries in this rerun used Rust 1.88.0. The earlier baseline used a different compiler. The control is the source snapshot before the RAM changes. The intermediate binary is the previously reported RAM release. Within each trial the three builds took turns running first. Each sequence started a fresh server. The origin added 4 ms per response and shared 512 MiB/s across all transfers. The original JP2s passed the fixture SHA-256 checks. The harness compared decoded full tiles with the reference images and compared reduced and preview pixels across builds.

### Isolated causes

On urban, with the same 16 KiB blocks and automatically planned batch, bypassing only the adaptive source-admission window cut median preparation from 74.9 to 46.1 ms. Both treatments fetched 52 ranges and 5.03 MB. The five runs with the window enabled registered 27–29 admission waits by the first full tile. Overlapping requests could finish in a burst, and the next sampling window was backdated to the start of an already in-flight GET. The same RTT could generate several successive low-throughput samples and halve the window repeatedly. River now accounts for each completion in a disjoint interval, discounts idle gaps, and requires a saturated window before reducing admission on a throughput drop. A deterministic test covers simultaneous completions and a pause between requests. [Admission ablation](../target/benchmarks/wmts-isolated-dispatch.json).

In a separate seven-trial urban comparison, both binaries included the same cache-block and parent changes. Applying only the scheduler correction moved first-full latency from 159.8 to 132.3 ms and preparation from 74.2 to 48.9 ms. Median cumulative source admission waits through the first full response went from 27 to 0. [Isolated scheduler timings and origin traces](../target/benchmarks/wmts-window-ablation.json).

On terrain, switching only the parent to the existing whole-packet path moved the child median from 218.9 to 146.9 ms in five isolated runs. The parent requested 11.44 MB in 11 GETs, versus 3.32 MB in 65 GETs with sparse selection. River now uses packet lengths to choose this reusable reduced view when it fits the remaining request allowance, the range cache, and 16 MiB. Larger reduced views keep sparse codeblock selection. For compatible single-layer remote images, the reader also estimates cache block size from compressed codeblock density, capped at 16 KiB, to limit per-block allocation and bookkeeping. The coarser alignment can fetch extra bytes. [Parent and block-size ablation](../target/benchmarks/wmts-isolated-focused.json).

### Matched navigation

Client latency in milliseconds. Each original-image row pools three trials on each of terrain, urban and texture. Each alternate-encoding row pools three trials on each of two layouts. Wire traffic covers the indicated phase. MB is decimal.

| Source and driver | Request | Original ms | RAM release ms | Fixed ms | Fixed GETs | Fixed MB |
|---|---|---:|---:|---:|---:|---:|
| Single layer, openjpeg | preview / preview | 62.8 | 63.9 | 65.7 | 10 | 0.80 |
| Single layer, openjpeg | preview / firstFull | 150.7 | 167.8 | 131.6 | 53 | 5.03 |
| Single layer, jp2grok | preview / firstFull | 167.2 | 176.6 | 154.1 | 53 | 5.03 |
| Single layer, openjpeg | zoom / parent | 240.3 | 206.6 | 233.1 | 12 | 11.55 |
| Single layer, openjpeg | zoom / firstFull | 159.4 | 193.6 | 157.9 | 52 | 3.47 |
| Single layer, openjpeg | cold / firstFull | 203.9 | 219.4 | 217.9 | 65 | 8.81 |
| Single layer, openjpeg | preview / sibling | 1.8 | 2.1 | 2.0 | 0 | 0.00 |
| Single layer, openjpeg | preview / vertical | 146.5 | 220.5 | 133.5 | 48 | 3.41 |
| Single layer, openjpeg | preview / distant | 124.8 | 135.7 | 114.8 | 48 | 5.29 |
| Single layer, openjpeg | preview / repeat | 2.1 | 2.2 | 1.9 | 0 | 0.00 |
| Single layer, openjpeg | fanout / concurrent | 811.0 | 872.2 | 807.1 | 149 | 14.06 |
| Multilayer, openjpeg | preview / preview | 48.4 | 75.0 | 75.0 | 19 | 3.20 |
| Multilayer, openjpeg | preview / firstFull | 109.8 | 68.0 | 66.0 | 0 | 0.00 |
| Multilayer, openjpeg | pause / firstFull | 105.3 | 2.1 | 2.1 | 0 | 0.00 |
| Alternate encodings, openjpeg | edge / firstFull | 133.0 | 153.0 | 135.4 | 46.5 | 1.81 |
| Alternate encodings, jp2grok | edge / firstFull | 104.3 | 150.6 | 105.6 | 44 | 1.87 |

| Original image | First full after preview, original / RAM release / fixed ms | Zoom child, original / RAM release / fixed ms |
|---|---:|---:|
| Terrain | 150.7 / 171.8 / 131.6 | 160.1 / 194.6 / 159.6 |
| Urban | 148.1 / 146.8 / 126.1 | 156.0 / 188.4 / 152.9 |
| Texture | 155.4 / 167.8 / 143.9 | 156.3 / 193.6 / 158.8 |

The harness completed 225 sequences and checked 1,296 responses. The default and Grok-enabled server suites each passed 330 tests with 16 ignored. The core suite passed one test. The measured fixed binary SHA-256 is `2c587a6cef6aedaf19bd3c8be07542951d9da4801c14a84803483c8a1d91b1f4`. [Raw timings, origin counts, and pixel hashes](../target/benchmarks/wmts-ram-navigation-repaired.json).

### OpenJPEG parent and zoom-child profiling

A matched four-trial rotation over the original terrain, urban and texture single-layer
fixtures compared the prior release, the MQ exchange rewrite alone, and the rewrite
with CPU leases sized for a large single-native-tile reduced packet view. Median
parent latency was 224.9, 214.5 and 156.1 ms. The corresponding child medians were
150.0, 146.4 and 146.2 ms. Median paired elapsed time was 376.7, 360.0 and 300.6 ms.
In a separate three-trial experiment, sparse reads helped the parent but moved the
paired median from 371.7 to 379.3 ms. The production path retains the dense parent.

Generate the colored wall-time breakdown with
`benchmark-wmts-ram.py --profile-zoom` using the usual before/after binary and
output arguments, then run `plot-wmts-driver.py measurements.json timings.png`.
The plot reads the adjacent log of native codec stages. It uses a common millisecond
axis and arithmetic means so each phase adds up to the displayed total. Source
preparation is teal, Tier-1 orange, inverse wavelet purple, other rendering blue,
and remaining elapsed time gray.

[Driver ablation](../target/benchmarks/wmts-driver-ab.json) ·
[Timing graphic](../target/benchmarks/wmts-driver-timings.png) ·
[Sparse-parent experiment](../target/benchmarks/wmts-driver-sparse.json) ·
[Independent raw component checks](../target/benchmarks/wmts-driver-planes.json).

A second comparison ran the final release directly against m1227 with four trials
per original image. Median paired parent and child elapsed time fell from 776.9
to 305.7 ms, with median sequence bytes falling from 49.01 to 15.02 MB.
The broad comparison against the immediately preceding release validated 864
responses and measured 391.6 to 314.7 ms for the same zoom sequence.
[Direct m1227 comparison](../target/benchmarks/wmts-driver-m1227-zoom.json) ·
[Full navigation matrix](../target/benchmarks/wmts-driver-final.json).

### Stable original offsets and native block reuse

Both OpenJPEG and the pinned Grok v20.4.15 bridge can read original object
offsets through River's version-pinned RAM range cache. Original-offset reads
share the request's wire budget and cancellation. Automatic selection uses
the original stream for a single cache-block object and preserves the existing
selective preparation policy for larger images. For qualification,
`RIVER_S3_ORIGINAL_RANGES=true` requests an original stream on supported
native CPU routes. On the terrain original, forcing this fetched 48.83 MB on
the OpenJPEG parent and generated 131 GETs on the Grok child. The default
selective routes fetched 11.45 MB and 3.41 MB for those requests.

The in-pod cache stores full-quality Tier-1 coefficients for original,
zero-origin, single-native-tile views with verified geometry. It is shared
across requests and threads. The source revision, component, resolution,
subband, block geometry and coding-pass identity must match. OpenJPEG, classic
Grok and Grok Mercury use separate representations. Mercury carries its cache
callback on the individual plan. Workers copy into their own mutable buffer
before transforms. The cache has 32 FIFO shards and a combined budget of
one sixteenth of the decoder reserve, capped at 256 MiB. Concurrent misses may
decode the same block before either publishes it. Cache entries use pod RAM only.

On the terrain, urban and texture originals, the zoom child hit 408 of 948
blocks on each driver. The later vertical neighbor hit 606 of 966, and the
full tile after the coarse preview hit 39 of 633. The new WMTS headers
`x-river-block-hits` and `x-river-block-misses` describe actual lookups.
`x-river-grok-native-ms` measures the bridge call, `x-river-grok-decompress-ms`
is the nested Grok decompression interval, and `x-river-grok-conversion-ms`
covers component-plane extraction and RGBA conversion after the bridge returns.
Encoded-raster hits omit these headers.

Four alternating trials per original and driver gave median parent→child
elapsed times of 176.2 to 166.1 ms for OpenJPEG and 206.8 to 194.0 ms for
Grok. Preview→full moved from 111.7 to 110.6 and 119.9 to 119.0 ms respectively.
The benchmark used a local 4 ms delayed origin capped at 512 MiB/s and validated
864 WMTS responses. The additional concurrent run checked 120 responses.
[Raw navigation](../target/benchmarks/wmts-stable-drivers.json) ·
[Timing splits](../target/benchmarks/wmts-stable-drivers.png) ·
[Independent preview and parent checks](../target/benchmarks/wmts-stable-drivers-reference.json) ·
[Concurrent requests](../target/benchmarks/wmts-blocks-concurrent.json) ·
[Direct original-offset experiment](../target/benchmarks/wmts-original-offset-terrain.json) ·
[Classic Grok qualification](../target/benchmarks/wmts-classic-blocks.json).

### WMTS requested tile and background sibling

The first full request and its horizontal neighbor now have separate raster-cache flight keys. The source, driver and output parameters still participate in each key. A foreground neighbor arriving during its speculative render waits on that same flight and uses the published tile. Speculation is bounded by local admission and RAM. On the original single-layer JP2s, scheduling the sibling after the foreground render also lets its source planner see the foreground coefficients. On multilayer sources, sibling preparation may overlap the in-flight full refinement. Native Grok can also decode that sibling with spare CPU admission. Concurrent OpenJPEG decoding on the multilayer workload delayed the requested full tile and was excluded.

The release comparison used 22 effective CPUs, a local 4 ms range origin with a shared 512 MiB/s body rate, and five alternating runs per driver on each of the six original files. The client immediately issued the next request on receiving the previous response and deferred statistics and pixel checks until after navigation. These are median milliseconds. Sequence columns are medians over complete sequences.

| Layout and driver | First full, old → new | From preview start through full | From preview start through neighbor |
|---|---:|---:|---:|
| Single, OpenJPEG | 81.7 → 55.9 | 122.4 → 97.8 | 123.8 → 144.3 |
| Single, Grok | 88.1 → 63.6 | 128.3 → 105.1 | 129.6 → 151.8 |
| Multilayer, OpenJPEG | 42.7 → 44.2 | 90.3 → 69.6 | 91.6 → 95.1 |
| Multilayer, Grok | 59.3 → 60.5 | 89.9 → 85.8 | 91.4 → 89.1 |

A separate scheduling-only versus scheduling-plus-worker comparison on single-layer OpenJPEG moved native decode from 40.6 to 20.6 ms and the complete first-full response from 69.8 to 49.5 ms. Source preparation stayed at 25 ms. The worker change applies the existing fair-share budget to full-resolution sparse and packet streams of at least 1 MiB at 512×512 pixels or larger. The isolated runs held transfers at 53 GETs and 4.31 MB in both conditions.

All 480 responses in the main comparison matched their pixel checks. An eight-client simultaneous-neighbor run served all 96 candidate neighbor responses via the encoded raster cache, with 72 waits above one millisecond. Raw measurements, per-image medians, independent preview checks, and the timing plot are under `target/benchmarks/wmts-background-release*` and `target/benchmarks/wmts-background-summary.json`. The isolated worker comparison is in `target/benchmarks/wmts-async-sibling.json`.

### Tier-1 decode during sparse payload reads

Baseline is the previous background-sibling release. The local range origin used a 4 ms delay and a shared 512 MiB/s body rate. Alternating runs used the unchanged terrain, urban and texture originals in both layer layouts. The client sent each request immediately after the preceding response. Times below are medians in milliseconds; whole-path timings use each observed end-to-end interval.

| Layout and driver | First full | Preview through neighbor |
|---|---:|---:|
| single, openjpeg | 56.7 → 50.0 | 148.5 → 136.3 |
| single, jp2grok | 62.6 → 49.0 | 149.6 → 134.8 |
| multilayer, openjpeg | 42.8 → 46.7 | 103.4 → 102.1 |
| multilayer, jp2grok | 60.0 → 60.2 | 90.1 → 90.9 |

The early Tier-1 path covers full-resolution single-layer, reversible Part 1 sparse reads with supported quantization and geometry. Multilayer results are controls. Completed coefficients are published to the source revision's driver-specific RAM cache. Exact source bytes are retained for the normal decoder when a worker, a cache entry or a complete codeword is unavailable.

| Off-center viewport path | Zoomed child | East hop | South hop | Entire path |
|---|---:|---:|---:|---:|
| single, openjpeg | 51.9 → 49.1 | 101.8 → 75.5 | 112.4 → 112.0 | 818.5 → 799.0 |
| single, jp2grok | 59.1 → 40.7 | 66.3 → 77.2 | 98.1 → 90.1 | 729.8 → 707.0 |
| multilayer, openjpeg | 54.6 → 55.4 | 48.8 → 49.5 | 153.9 → 153.0 | 681.7 → 736.1 |
| multilayer, jp2grok | 82.2 → 69.2 | 55.1 → 56.9 | 59.0 → 62.0 | 523.9 → 512.6 |

The viewport path requested an overview, an off-center parent, its full-resolution child, three tiles east, three tiles south, and a backtrack. The additional same-build comparison toggled only horizontal sibling prediction. Trials measure HTTP completion, without browser paint or gesture cancellation.

| Same build, automatic sibling on → off | Entire viewport path |
|---|---:|
| single, openjpeg | 766.0 → 469.3 |
| single, jp2grok | 686.6 → 422.2 |
| multilayer, openjpeg | 723.0 → 390.4 |
| multilayer, jp2grok | 483.2 → 445.0 |

[Measurements](../target/benchmarks/wmts-arrival-summary.json) · [Timing bars](../target/benchmarks/wmts-arrival-timings.png) · [Independent preview and parent checks](../target/benchmarks/wmts-arrival-viewport-pixels.json)

# River resource sizing

`GET /api/diagnostics/resources` shows the cgroup limits, CPU affinity, startup reservations, cache ceilings and occupancy, active workspace reservations, and pressure adjustments. The process RSS and cgroup working set are measured separately. Each replica maintains its own RAM caches. Its persistent cache location normally belongs to its own volume.

Automatic sizing allocates cache ceilings after runtime, native workspace, source request, and temporary-buffer reservations. Allocations are lazy. More pod RAM mainly retains more previously requested ranges, headers, indexes, plans, and raster results. CPU quotas and per-source request policies still govern parallel work. Numeric overrides remain ceilings. When their aggregate exceeds the shared allowance, the diagnostic reports the configured values and smaller effective targets. Set `RIVER_RESOURCE_ADAPTIVE=false` to keep the startup targets fixed. `RIVER_MEMORY_BUDGET_BYTES` can lower the allowance and `RIVER_CACHE_MEMORY_BYTES` can lower the shared cache ceiling.

Technical index RAM and disk entry caps are derived from their byte budgets. Override them with `RIVER_S3_INDEX_MEMORY_ENTRIES` and `RIVER_S3_INDEX_CACHE_ENTRIES`. Index, raster and asset locator disk ceilings are separate from RAM. Cache population follows requests to individual objects. Original URL access refreshes mutable object metadata after `RIVER_S3_METADATA_TTL_MS` (30 seconds by default). Version IDs retain their format, and reloads validate the version before serving it.

For multispectral input, River validates the actual SIZ fields and supports up to 16 components of at most 16-bit precision. The default display uses the first three components and preserves declared RGBA opacity. This is a display policy. Use scientific band tools for band selection. Invalid optional TLM indexes can fall back to bounded SOT discovery using the existing request budget.

Adaptive source planning opens a bounded initial prefix window on eligible coarsest previews under projected range pressure. Set `RIVER_S3_LL_INITIAL_BYTES=0` to disable it or set a byte ceiling up to 16384. Eligible whole-image index walks can sample up to 256 KiB per pilot request. `RIVER_S3_INDEX_PILOT_BYTES` overrides the sample size within 64–256 KiB. Explicit range limits and the existing byte and time bounds still apply.

Eligible classic TLM previews can combine the tile-part header and initial LL payload under projected range-request pressure. Set `RIVER_S3_TLM_LL_FUSED_BYTES=0` to disable the fused read. Explicit LL-window settings take precedence.

Adaptive whole-view progressive previews can start index discovery after four reads when observed latency is at least 7 ms. Set RIVER_S3_PROGRESSIVE_EARLY_DISCOVERY=0 to disable. Explicit range limits and existing probe budgets apply.

Whole-view Grok progressive previews share preparation with the final viewport where supported. Set RIVER_GROK_PROGRESSIVE_PREFIX=0 to disable.

Progressive lookahead can sample a bounded range during stall backoff. Set RIVER_PREFIX_RECOVERY_PROBE=0 to disable recovery probing. Existing cache, transfer-cost, and range limits still apply.

Stable and LTS m1148 stage whole-image speculative index probes and fetch the remaining window when a seed is absent. Set `RIVER_S3_STAGE_WHOLE_INDEX_PROBES=0` in the backend environment to restore full windows for whole-image probes. On the cold 979 MB PNG overview, staging reduced origin traffic from 19.64 MB to 16.88 MB. Matched tests did not establish a latency speedup.

Stable and LTS m1162 can select JP2Grok for an unforced reduced whole-image streamer preview of an eligible classic 8-bit RGB RPCL source. The bounds are 16 native tiles and 16,777,216 source pixels, with a zero image origin. Set RIVER_GROK_RPCL_PREVIEW=0 in the backend environment to restore the OpenJPEG default. Explicit driver choices take precedence.

Stable and LTS m1165 read one adjacent block during SOT discovery for eligible unindexed tiled RPCL overviews. Set RIVER_S3_SOT_ADJACENT_BLOCK=0 to restore the earlier probe. The 16-tile cold preview used 32 GETs instead of 44 with identical bytes and pixels. The matched release run did not establish a latency gain.

Stable and LTS m1167 also include the adjacent block in the indexed SOT batch for eligible tiled RPCL overviews when TLM entries identify tile numbers. Set RIVER_S3_TLM_ADJACENT_BLOCK=0 to restore the earlier indexed probe. The matched indexed fixture used 32 GETs instead of 43 with identical bytes and pixels. Median latency was effectively unchanged. Invalid TLM lengths fall back to validated SOT discovery.

Stable m1169 can begin speculative index discovery after four observed reads for eligible distant standalone overviews in adaptive mode. Set RIVER_S3_EARLY_STANDALONE_DISCOVERY=0 to restore the previous calibration. On the cold 979 MB overview, origin traffic dropped from 16.88 MB to 15.50 MB with exact pixels. The matched release run did not establish a latency gain. LTS remains m1167.

Stable and LTS m1174 merge TLM marker and reduced-prefix reads for eligible classic whole-image overviews with up to 512 typed TLM entries. Set RIVER_S3_TLM_REDUCED_PREFIX=0 to restore separate reads. On the indexed 979 MB fixture the merge cut GETs from 676 to 340 with identical bytes and pixels. Paired cold latency improved 25.9% against Stable m1169 and 27.0% against LTS m1167 at 7 ms configured origin delay.

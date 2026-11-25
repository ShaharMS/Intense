# River caches and deployment learning

The nine RAM categories represent stages of reusable work, not nine copies of an
image. A request may touch several categories, bypass unsupported paths, or stop
at an encoded-result hit. Their occupancy is not process RSS: active requests may
retain payloads after eviction, and decoder/native/request memory has reservations
outside cache targets. Persistent caches and external Redis/Valkey are separate.

| Cache | Stored item and size basis | Why retain it | External tier |
| --- | --- | --- | --- |
| ranges | Compressed block payload plus key/map overhead; block size depends on read planning | Avoid fetching the same source bytes | Local |
| headers | Main header, container data and marker tables; variable by component/tile-part count | Avoid another header read | Local |
| indexes | Tile-part records, identity and vector capacity; grows with parts, not image area | Avoid rediscovering byte offsets | Compact validated indexes/prefixes can use Redis/Valkey |
| parsed_headers | Parsed tile headers and their tables; variable marker/packet structure | Avoid parsing retained bytes again | Local |
| packet_plans | Selected packet spans and plan structures; varies by viewport/layers | Avoid repeating packet selection | Local |
| prepared_prefixes | Prepared codestream prefix plus selection key and bookkeeping; eligible payload capped at 4 MiB | Reuse decoder-ready inputs | Local |
| native_tiles | RGBA pixel capacity plus bookkeeping; approximately width × height × 4 bytes for pixels | Avoid native decoding | Local |
| raster_results | Encoded PNG/WebP/AVIF/JXL payload plus keys and bookkeeping; content dependent | Avoid preparation, decoding and encoding for repeated views | Versioned single encoded tiles can use Redis/Valkey |
| asset_metadata | Demand-loaded image handle, manifest and shared metadata ownership; base accounting includes 4096 bytes plus variable structures | Preserve prepared state across requests | Local; compact locators can persist separately |

Example: a 512×512 RGBA native tile has 1 MiB of pixels before overhead. An encoded
512×512 tile has no fixed expected size; smooth imagery and random texture can
compress very differently. Use averageRetainedEntryBytes and occupancy from
`/api/diagnostics/resources` for the actual workload. Accounting is an estimate of
retained cache data, not allocator-exact RAM measurement.

## Request paths

- Cold image: header and metadata preparation, applicable index/packet plans,
  compressed payload reads, decode, then output encoding. Specialized paths may
  populate prepared prefixes or native tiles.
- Zoom after preview: prior metadata/index/byte state can be reused, but a preview
  does not necessarily contain the high-frequency packets required for zoom.
- Exact repeat: an encoded tile hit can bypass preparation and native decoding.
- Shared hit on a cold replica: origin version validation can still be necessary.

Hits/misses/evictions are per cache operation, not per HTTP request. An image with
many blocks can produce many range-cache hits during one request. bytesOrWorkReused
has cache-specific units: encoded/range bytes for payload caches, records or work
items for some metadata caches. Do not rank unlike units as if they were bytes.

## Eviction and learning

Specialized caches retain their own recency and validity rules. There is no universal
"evict cache number 1 first" ordering. Under pressure, the resource coordinator
reduces effective limits and each cache reclaims its own eligible entries. Active
references may delay release. Object-version validation takes precedence over reuse.

Initial shares use established weights. Once per minute the learner observes hits,
misses, evictions, occupancy and measured producer costs reused on cache hits. It
ranks eligible caches by estimated work saved per resident byte, weighted by the
fraction of hits with a measured cost. At least eight measured hits and 32 total
hits, eviction pressure and 75% occupancy are required to receive budget. Donors
must have three no-hit windows, or measured substantially lower benefit with spare
capacity and no evictions. Unknown benefit is not interpreted as zero benefit.

Seven categories currently attribute measured producer costs: range reads, header
reads, tile-header parsing, packet plans, prepared prefixes, native decoding and
raster decoding/encoding. Indexes and image metadata share incremental ownership
and do not yet have reliable per-entry time attribution; their diagnostics report
`unavailable` with the reason. Untimed paths in other caches report partial coverage.
Each cache exposes `benefitMeasurement`, sample counts and
`estimatedWorkAvoidedNanoseconds`. Costs describe previously measured work that a
hit reused; they are estimates, not end-to-end speedups. Nested costs overlap and
must not be summed across caches.

A cache keeps at least half its initial allocation; growth is capped at twice that
allocation. Each donor and recipient changes by at most 5% of its baseline per
minute. The total automatic budget is conserved. Explicit per-cache settings are
untouched. A new process starts afresh; no global model assumes all installations
have the same workload. Disabled adaptation pauses learning.

## Measured source overfetch

`/api/settings/read-policy` reports per-source `actualOverfetch` for payload
preparation and `metadataOverfetch` separately. The numerator counts successful
origin response bytes, including duplicate reads. The denominator is the unique
intersection of those reads with consumer-requested byte intervals before block
alignment and optional gap filling. Cache-only demand does not inflate it. This is
not the theoretical minimum codestream entropy: explicitly requested read-ahead
can still count as demand. Retries with unaccounted response bodies, failed/partial/cancelled transfers and interval-accounting
overflow are reported as unmeasured and cannot train the controller. Partial failed
response bytes are not fully observable. Accounting retains at most 8192 intervals
of each kind per preparation and no payloads or keys.

In adaptive mode, after the configured minimum sample count, eight-sample windows
can lower the optional gap budget by 0.125 when measured amplification exceeds the
configured target by 10%. Three low-amplification windows are required for each
0.125 recovery step. The learned ratio stays between 1 and the configured ceiling;
explicit byte, request, concurrency and range limits still apply. Observe mode
reports recommendations without applying them; disabled mode does not learn.
Metadata probes do not train the payload controller. Required alignment and merges
can exceed the optional target; the target is not a hard bound on all wire bytes.
A reduced allowance therefore does not itself establish fewer bytes or faster reads.

Use RIVER_CACHE_LEARNING=false or the live settings endpoint to compare fixed initial
shares with learning. A benchmark that finishes before the sampling windows elapse
cannot establish a redistribution benefit. Compare steady workloads and changing
workloads separately, with the same memory budget and identical image requests.

## Multiple external servers

Each key hashes into one of 4096 routing buckets. A weighted rendezvous table maps
buckets to stable endpoint IDs; request routing performs one hash and a table lookup.
Endpoint order and credentials do not affect placement. Membership/weight changes
move affected keys and can cause temporary misses. Entries are not migrated or
replicated automatically. Keep endpoint membership consistent across replicas.

Local hits remain local. Remote tile batches group by destination and are submitted
before waiting, with one total timeout deadline. Reads, fills and values are bounded;
network errors are misses and connection retries back off. All endpoints share a 16 MiB
pending-fill ceiling and the configured fill-rate ceiling; adding servers does not
multiply those budgets. Each endpoint has worker
connections, so endpoint count still has resource cost. Server memory/key counts are
whole-server measurements, not an exact River namespace size; River does not scan
keys to compute that size. Inspect per-endpoint diagnostics without exposing secrets.

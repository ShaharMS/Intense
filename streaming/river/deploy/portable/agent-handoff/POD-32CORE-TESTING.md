# River 744: 32 CPU / approximately 62 GB pod, nearby S3

This is a starting profile and a bounded test plan, not a measured optimum for
the organizational pod. Use BUILD-MANIFEST.json for the exact immutable image.
Current Stable/LTS is 744; consult release.json for the current experimental Latest. 744 was qualified on 2026-11-02 for12.5-25% smaller lossless PNG responses with exact pixels and unchanged source I/O. This is a response-bandwidth improvement, not a universal latency speedup. 698 is the immediate previous version. Read release.json and PERFORMANCE.json for exact evidence and limitations. Earlier 651/663/680/458 references below describe historical comparisons, not retention pins. Organizational S3 qualification remains open. No original imagery may change.

## Measured worker scaling on 698

The same 698 image was tested with a 16 CPU allocation on both sides, one admitted
decode, a 64 MiB byte cache, and OpenJPEG maximum workers changed from 4 to 16.
Six balanced pairs covered all six frozen preview/deep requests, exact pixels
and zero additional source I/O on exact repeats. This is configuration evidence,
not a new binary or an organizational measurement:

- 50 MB preview: 400→237 ms; all six pairs faster.
- 979 MB preview: 411→386 ms; no clear gain across pairs.
- 11 GB preview: 774→357 ms; all six pairs faster. Decode median: 602→187 ms.
- Deep tiles showed no clear gain. The 50 MB deep median was223→239ms, with
  mixed pairs; do not treat this as a universal improvement.

GET counts and source bytes matched for every pair. The11GBpreview remained
8GETs/5,619,826bytes. The source was unshaped local HTTP with warm origin OS pages,
not organizational S3. Read the separate worker comparison in PERFORMANCE.json.
Do not compare these16CPU figures directly with the earlier4CPU code comparisons.

The 32 CPU starting profile below already specifies16maximum workers per decode.
Keep this as a cold-latency candidate. The experiment admitted only one decode;
it does not establish the best concurrency/throughput profile. Run the suggested
concurrent-client sweep before selecting production capacity settings. River's
existing request sizing and active-work budget can reduce actual workers below
this maximum for small or concurrent jobs.

## Capacity screen on 698

A separate descriptive screen used 16 CPU / 8 GiB, one shared source connection
limited to 8 operations, a 64 MiB source byte cache and a 256 MiB encoded cache.
Distinct object keys alias the same frozen bytes; every cold response performed
source I/O and decoding. All six preview/deep cases across the three sizes passed
four-client pixel/repeat checks. The11GBpreview also covered1/4/8/16clients.
No OOM kills or source-listener drops were observed in the completed screens.

For16simultaneous11GBpreviews, one cohort per profile measured:

- 1 decode slot /16maximum workers: 4.23 s for the cold cohort;759MiB peak.
- 4 decode slots /16maximum workers: 3.10 s;927MiB peak.
- 8 decode slots /4maximum workers: 3.26 s;896MiB peak.

Keep4/16as the starting point. These are screen observations, not statistically
established wins or a measured32CPUoptimum. Confirm with representative distinct
organizational images and sustained traffic before changing capacity settings.

Client placement matters. A16-clientcachedPNGburst contains approximately83MB.
One placement check took858ms through the Windows Docker port path versus88–94ms
from a2CPU Linux client, with identical bodies and no source reads. The Linux
cold burst still took4.03s, so concurrent cold decoding remained a server cost.
Do not label cached client/network transfer time as decode or source time. The
original180secondattempt is preserved as partial evidence; completed bounded
screens and the placement check are documented in the capacity addendum.

680 also enables spatial packet selection for eligible ten-layer reversible
sources when `RIVER_S3_PRECINCT_ROI=true`. Its initial local diagnostic has a
14 MB synthetic RPCL/TLM/PLT source, not organizational imagery: full-resolution
tile medians improved from 377–428 ms to 231–292 ms versus 651, with exact pixels
and zero extra reads on exact repeats. Preview timing did not clearly improve.
Read PERFORMANCE.json for all conditions and limits. The historical comparison
used 651; use its archived evidence rather than retaining that old image by default.
Do not infer an 11 GB cold-preview result from this diagnostic. For the first
organizational A/B, keep the same pod resources, source, settings and tile URLs
on both images; tune the settings below only in a separate comparison.

698 batches payload demand across selected tile-parts when the plan fits the
existing per-request metadata (4 MiB default) and retained-payload (8 MiB) bounds.
It also reconstructs temporary SOT markers from already-validated fields instead
of fetching them again. Larger/fragmented plans retain per-part handling; source
byte/request allowances are unchanged. Relative to 680, the same synthetic
full-resolution diagnostic measured 253→182 ms, 309→188 ms and 228→144 ms across
six balanced pairs. GETs changed 27→13, 24→5 and 22→7, respectively. Merging used
0.86–1.24 MB rather than 0.52–0.69 MB per tile, within the existing 2x bound.
Preview timing did not improve. The separate three-size gate versus then-stable458
does not demonstrate a broad gain; the11GBpreview median was552→586ms in that
run. See PERFORMANCE.json; these are separate comparisons, not cumulative gains.
Keep the deployed control as well as 698 while collecting organizational results.
Do not raise request/byte limits to make a slow path pass. Capture read-policy
observations and a request-correlated source trace first. Source diagnostics
separate validated TLM index entries from real GETs; never add those counts.

## Preserve before tuning

Read AGENTS.md and inventory the EXISTING workload, CPU requests/limits,
memory quantity, UID, service account, mounts, config/cache PVCs, Secret/CA refs,
routes, environment and current API settings. Privately back up configuration.
651's configuration export is credential-bearing; keep it out of shared results.
Do not recreate S3 connections, replace their IDs, import an example whole
configuration, delete caches/volumes, or replace the existing deployment manifest.
Keep S3 endpoints, bucket/prefix, addressing, retries and credential/provider setup.

Confirm the running container actually sees the intended CPU/memory resources;
record cpu.max, memory.max, cpuset/affinity and throttling where available. A32 CPU
limit with a lower CPU request is not a guarantee of32dedicated cores under load.
Preserve the pod's actual memory quantity rather than silently converting GB/GiB.

Verify the existing S3 connection uses the intended internal/local endpoint.
Colocation does not prove low GET latency. Check River-observed header latency,
body rate, retries and source load; don't substitute a public URL just for tuning.

## Starting environment: balanced profile

Review/merge pod-32core.env.example into the existing River container env.
An .env file alone does NOTHING to Kubernetes; retain all unrelated env/Secret refs.
Environment/cache/concurrency changes require a pod restart; API settings persist.

- 4 admitted decodes,up to16 OpenJPEG workers per decode,8 source preparation slots.
- OPJ_NUM_THREADS=0is deliberate: River applies its bounded per-codec worker budget.
 Do not set this global initialization pool to32 as well.
- 2 GiBsource-range cache and2 GiBrendered-image cache;128 MiBmain-header cache;
 64 MiBprepared-prefix cache. These four caps total about4.2 GiB,NOT a global RSS
 bound. Decoder workspaces,in-flight buffers,metadata and OS page cache add memory.
- Keep existing disk-cache/PVC settings. Larger caches help repeated/working-set
 loads; they do not prove a faster first touch of an unseen object.
- Keep existing explicit retries,byte/request caps and compatibility limits.
 Do not raise whole-file compatibility limits to the image's10-15GBsize to hide
 a selective-reader failure. More RAM is not permission to fetch whole images.
- Use OpenJPEG as the comparison baseline. Test other drivers separately with
 fixed resource settings; don't mix driver changes into the concurrency sweep.

Check structural flags already supplied by the bundle: precinct ROI,multipart
PLT/prefix,single prefix,exact support,streamed PLT ROI,index-only prefix. Record
an existing explicit false before testing a change; don't erase custom overrides.

## Persisted read policy: deliberate merge

First GET /api/settings/read-policy and save its settings object. The helper
prints a proposed merge by default; --apply persists it and records prior values.
It changes only this endpoint,preserving indexHeaders and other existing fields;
it never sends sources:[] or calls configuration import.

```sh
python3 apply-nearby-read-policy.py --api http://127.0.0.1:8080
python3 apply-nearby-read-policy.py --api http://127.0.0.1:8080 --apply
```

The proposed values are adaptive,minimumSamples=8,maxSourceRequests=8,
maxOverfetchRatio=2,serialTransport sdk,refreshPayloadCosts false. Record the
intentional differences from existing settings before applying. If an existing
source cap is below 8,keep it for the initial baseline and test 8 as a separate run.
API values override ENV fallbacks; verify effective values after restart.
An existing RIVER_S3_RANGE_OVERFETCH_RATIO can further cap the effective ratio.

maxSourceRequests is concurrent operations per configured source per instance,
NOT GETs per image or requests/second. Multiple IDs/replicas multiply aggregate
load. Do NOT set RIVER_S3_REQUEST_RANGES=8as a synonym; it is a different hard cap.
refreshPayloadCosts=false does not stop learning. It disables an extra mature-
source refresh step; test true separately only for a demonstrated variable-source
problem. Preserve retries on each S3 connection; don't raise retries as tuning.

Learn from real approved requests on the SAME source ID. Observe samples,
throughputSamples,bodyThroughputSamples,latencyMs,recentHeaderMinimumMs,
bodyBytesPerSecond,lastDecision and admissionWaits. Some planners need at least
three 32 KiB bulk samples as well as the sample threshold. Do not scan a bucket or
download a large image merely to train the planner. Measurements reset on process
restart; persisted settings and disk indexes are different state.

## Small tuning matrix: one variable group at a time

Keep32 CPU/the same memory budget and cache sizes fixed. Start with balanced 4/16.
Compare latency-oriented1 decode / 32 workers and capacity-oriented8 decodes / 4 workers.
Keep source preparation slots 8 initially; test 4 or 16 only after choosing decode
settings. The worker value is a maximum; River may assign fewer to small jobs
and shares a bounded pool among concurrent jobs. Don't set every limit to32.

Then compare S3 caps 4, 8, 16under the chosen compute profile. Use the helper's
--source-requests option with --apply. Prefer the lowest cap that meets latency
and capacity goals without growing source waits,retries,503s or bandwidth spikes.
Compare sdk versus auto separately using --transport auto. Keep SDK if auto is
unsupported or adds variance; physical attempts/native serial counts show use.
Keep maxOverfetchRatio=2initially; don't combine a 4x overfetch test with other changes.

WebP method 0 is an OPTIONAL separate encoding-effort test. Read/save the existing
GET /api/settings/encoding value,then PUT {"webpMethod":0} to that endpoint and
verify persistence. It may increase response size. Use identical PNG output for
decoder-focused River/TiTiler/GDAL/Kakadu comparisons to remove this confound.

At each profile test 1, 4, 8 and 16 concurrent clients with bounded approved images;
report p50/p95,successful images/sec,RSS/peak,CPU/throttling,source GETs/bytes,
retries/errors and source admission waits. Don't choose a profile on one median.
Use the approximately 50 MB, 900-1000 MB, and 10-15 GB cases for previews AND deep tiles.

## Cold,learned-source and repeated loads

Do not clear production caches. Use a separate benchmark deployment and empty
test cache volume,with a preserved/private copy of configuration and the same S3
IDs/Secret references. Restart alone does not clear persistent indexes or raster
and disk-compatibility caches. Keep these conditions distinct:

- Fresh process/empty test caches and first source connection.
- Unseen object on a learned connection,using the same source ID.
- Exact repeated request,and nearby/new tiles within an already-open image.

For comparisons match source object/ETag,pixel ROI,resolution level,output size,
quality layers,resampling,bit-depth scaling,format and edge padding/alpha. The same
z/x/y string need not mean the same pixel window across services. Run the client
inside the pod/network near River; don't attribute workstation/TLS route latency
to decoding. Verify200+image content and decoded pixels; record422/timeouts and
their complete JSON/error bodies as failures,not fast successful responses.

## Bounded diagnostics; normal timing without profiling

Use RUST_LOG=river_server=info,tower_http=warn for readable normal logs.
Trace one isolated request: PUT /api/diagnostics/source-reads {"enabled":true},
DELETE the same endpoint to clear its ring,make the request,GET/save the trace,
then PUT {"enabled":false}. Check eviction/partial counters. Count actual wire
attempts including retries and opening+preparation phases; don't add logical and
physical events together. For authoritative totals use S3 access metrics too.
Record all x-river-*headers,especially route,queue,source,decode,encode/server time.

For a separate stage diagnostic set RIVER_PROFILE_SOURCE=1and restart,then
UNSET it before normal timing. Setting it to0does not disable presence-based
profiling in651. Don't expect the experimental fine-grained RIVER_PROFILE_CODEC
instrumentation in this image. Console fields are split/correlated at 160 UTF-8 bytes.
Never put credential-bearing configuration or signed URL tokens in shared logs.

651's sequential PLT early-stop route is single-quality-layer only. In 663 it
also supports full-quality multilayer RLCP/RPCL on eligible single-native-tile,
reversible PLT streams without TLM. Partial-quality and other layouts retain
their previous routes. Consult PERFORMANCE.json for the measured cases.
Missing TLM/PLT is not itself
unsupported input,but some layouts still hit bounds or require many GETs. Report
route and file markers; don't fix those failures by quietly downloading everything.

Deliver: exact image ID,effective env/read policy,resource/endpoint placement,
intentional changes,preserved S3 IDs,matched cases/cold definitions,raw timings,
wire GETs/bytes,pixel/error outcomes,best measured profile and rollback record.
Use release.json for current channel IDs. Historical candidates remain historical evidence. This guide does not claim
organizational qualification or a universal optimal configuration.

Rollback a policy change by PUTting the saved record.before object to /api/settings/read-policy; do not PUT the entire record. Run the helper from a writable directory or choose a private writable --record path.

## Historical 698 qualification and retention

698 held Stable/LTS/Latest before744. The frozen synthetic multilayer new-tile workload qualified directly against 294 and 458, with exact pixels and zero-read repeats. The neighboring-tile latency fell 697 to260ms versus294 and658 to224ms versus458 across six balanced pairs. This is not a claim for arbitrary11GBcold previews. All three primary source sizes retained pixel/repeat correctness; raw slower medians and uncertainty are in PERFORMANCE.json. No older local image is required as a permanent pin. Future builds advance Latest first; Stable requires a verified stable improvement and LTS a verified concrete >=10% substantial-workload improvement under matched conditions.

## Current 744 qualification

744 changes only fully opaque PNG encoding from RGBA to RGB. Any partial/zero alpha and transparent edge padding remain RGBA with exact alpha values. All six unchanged primary requests retain exact pixels and source I/O; exact repeats add zero reads. Encoded response sizes fall12.5-25%. The11GBpreview PNG falls5,170,488to4,521,891bytes.286unit tests and configuration-preserving upgrade from698 pass. Paired end-to-end times are mixed, so no general cold-latency gain is claimed. WebP,AVIF,JXL and original imagery are unchanged. RGB encoding uses an additional3bytes/output pixel for its temporary buffer; the older698capacity screen above is not a new744capacity measurement. Read release.json and PERFORMANCE.json for current IDs,raw evidence and limitations.

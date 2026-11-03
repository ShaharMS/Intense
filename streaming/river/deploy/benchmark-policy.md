# Fixed performance gate, version 2 (three sizes)

The previous reports are individual experiments, not a cumulative performance series. Some change source layouts, driver selection, baseline checkpoints or network conditions. They cannot be chained into an end-to-end speedup. Historical reports remain evidence for their stated scope only.

## Short feedback loop

Use tools/benchmark-fixed.py against two already-built instances. A frozen JSON manifest pins every file SHA256, independent reference PNG SHA256, exact tile geometry and driver. Start with three cases: small image preview, large image preview and deep tile of that same large file. Keep difficult unindexed/multipart imagery represented. Freeze this manifest before optimizing; do not replace a slow case. All comparison versions use the same manifest hash.

Four alternating, order-balanced paired rounds, each followed by one exact repeat, are the screening gate. Restart each dedicated River container before its timed cold request. Source registration, readiness and restart are outside the request timer, but registration must make ZERO origin requests. Every source GET/HEAD and response byte is counted from registration through completed response. Cold means fresh River process and source identity; origin OS cache remains warm. This is not a claim of physical-disk cold or organizational S3 latency. Timing is client HTTP start to complete response body, not just first byte. Pixel decoding and validation occur outside the timer. Repeat source IO is reported separately.

Set a 180-second screening deadline. A timeout is a failed/incomplete run, never a fast result. No encoders, builds, test matrices or competing load during measurements. Run correctness tests relevant to the code change separately, once, before release. Do not build/export a new checkpoint for each micro-experiment. Reuse trial tags.

## Claims and promotion

### Repeatability calibration (2026-10-29)

The identical-335-image A/A audit produced an apparent 5.1% median improvement
on the 11 GB deep tile with no code change. A six-pair adaptive control also
selected a different GET/byte plan in one pair; an observe-mode control retained
identical source IO but still showed substantial individual timing variation.
Evidence is preserved in `reports/kakadu-io-m303/benchmark-calibration-m336.json`.
Do not reinterpret small historical median shifts alone as verified code wins
or regressions. Do not delete or replace their raw measurements.

For short diagnostic feedback, `tools/benchmark-controlled-cold.py` performs one
correctness/repeat smoke across all six frozen requests, then six to twelve
balanced pairs only on explicitly selected affected cases. It records cgroup
CPU/throttling, source observations and exact source-read traces outside the HTTP
timer. Use `--read-mode observe` to investigate CPU/measurement effects without
applying adaptive plans, and `--read-mode adaptive` for the distinct end-to-end
adaptive workload. Report IO variation separately; equal IO is not proof of a
code effect. The helper accepts only the existing Compose project `river` replicas
`replica-1` through `replica-4`. Stable/LTS instances stay protected. Fixture
origins must reuse a replica slot; see `deploy/LOCAL-COMPOSE.md`.

Six pairs allow a two-sided exact sign-permutation probability below 0.05;
four pairs cannot (their minimum is 0.125). This is supporting evidence, not an
automatic promotion criterion. A focused run's full-suite smoke is correctness
evidence only: it does not replace the mandatory paired three-size release gate.
Apply the current Stable/LTS rules below, including the 10% substantial-workload LTS threshold.

Example for an affected frozen case:

```powershell
python tools/benchmark-controlled-cold.py --baseline-container replica-2 --baseline-api http://127.0.0.1:18083 --candidate-container replica-1 --candidate-api http://127.0.0.1:18080 --origin-container replica-4 --origin-control http://127.0.0.1:18085 --case gb11-deep --pairs 6 --read-mode adaptive --output reports/controlled-comparison.json
```

Report every fixed case: baseline/candidate median milliseconds, all four paired samples, latency reduction, GET/HEAD counts, source bytes and exact pixel match. Show cold and repeat separately. An aggregate must not hide a regression. Four pairs are a screen, not statistical proof: a marginal or inconsistent result requires another short run before claiming a speedup. Never report two decimal places as evidence of a sub-percent gain.

Stable advances on each reproducible improvement over CURRENT stable, with correct pixels and no unresolved regression in the fixed cases. LTS advances whenever a qualified candidate demonstrates at least a 10% concrete improvement versus CURRENT LTS in a substantial workload under matched conditions. A frozen supplemental workload may qualify when its geometry, source hashes, conditions and independent pixel references are recorded before comparison; it must not replace the mandatory three-size regression suite. Name the qualifying operation and metric explicitly and resolve reproducible regressions; unchanged previews must not be described as faster. For latency, candidate time must be at most 90% of baseline (at least 1.111x speed ratio). This supersedes the former 20% threshold (user correction, 2026-11-02). If a comparison version fails, report a correctness improvement, not a timing speedup.

Publish image IDs, manifest hash, effective server settings, machine and source/network description with every claim. Never compare differently configured images without explicitly calling it a configuration comparison. Near-origin and shaped-network results are distinct suites. Use a fixture origin in a Compose replica slot and record its measured header latency, aggregate body throughput and pacing model. An unshaped local HTTP Range origin must not be described as measured real S3 performance. Full progressive WS and multi-instance capacity are separate qualification workloads.

Record resolved startup sizing, not just `docker inspect` environment values or
Rust fallbacks: the packaged entrypoint applies its auto profile before starting
River. `effective_sizing` reads the server's startup fields. Sizing experiments
must reject settings that resolve to the same effective value. The attempted
five-decode experiment on270 was such a no-op: the packaged baseline already used
five slots. Its runtime audit is `five-decodes-effective-audit.json`; it is not a
five-versus-two performance comparison.

Generate an offline UI/backend ZIP after qualified gains. No web upload. Update stable/lts aliases, releases.json and retention pins together after qualification. Resolve current aliases live and use `deploy/releases.json` as the release record; historical experiment baselines do not qualify a new release.

## Historical v1 suite (through checkpoint263)

The v1 manifest remains in `reports/fixed-gate-v1/manifest.json`. It contains Bretagne (5.39 MB) and the existing unindexed, 16-bit, multi-native-tile 978.81 MB source. The current release gate is v2 below. The large preview uses reduction 5, and the nonblank deep tile covers source coordinates 20480,5120 through 20992,5632. This suite does not replace a separate 11 GB comparison.

Run an A/A control with `--comparison control` on identical images. Normal comparisons require the baseline container to match the live `river:stable` alias; use `--comparison lts` for the LTS check. The screening report never auto-promotes a release. It records the image IDs, environment fingerprints, runner hash, full samples, pixel validation and timing headers.

The initial completed comparison and independent confirmation took about 28 seconds each. Both found regressions in checkpoint 258 versus stable 228 on small preview and the nonblank deep tile. The large-preview delta was not sufficient to establish a gain. No new release promotion is justified by these results.

## Mandatory three-size suite from 2026-10-22

Every candidate comparison must include all three fixed source files: 54,776,951 bytes (50 MB class), 978,812,130 bytes (900-1000 MB class), and 11,123,320,946 bytes (10-15 GB class). Each file has a full-image preview and a nonblank full-resolution deep tile, each followed by an exact repeat. The 979 MB source bytes and request geometry remain unchanged from v1. The other two sources are generated fixtures; the 11.12 GB fixture is a physical JP2 file independently decoded by Grok, not a virtual HTTP representation.

`deploy/benchmark-suite.json` points to the frozen v2 manifest and the current stable baseline, resolved through `deploy/releases.json`. Run `python tools/benchmark-fixed.py --manifest reports/fixed-gate-v2/manifest.json` with the baseline/candidate API URLs, dedicated container names and output path. Hash each unique source once per run, rather than hashing the same 11 GB file separately for preview and tile. File size and modification time are checked while serving.

Report all six cold medians, repeated-load medians, actual GET/HEAD counts, bytes and pixel checks. Do not infer one size from another or replace the slow 11 GB case. Driver, geometry, quality, source layout and output dimensions are pinned. If source size/layout or requests change, create a new suite version and rerun both comparison versions. All three source sizes are mandatory even when a change targets only one size.

The first v2 same-image control took about85 seconds. All48 cold requests and their repeats matched references; repeats performed no source IO. OS caches were not flushed, so these are River-process/source-cold tests and are not physical disk-cold measurements. Size alone does not determine latency: the 979 MB source is multi-native-tile and unindexed, while the generated 50 MB and 11 GB sources have PLT indexes. No organizational Kakadu parity is inferred.

The recorded263 baseline is descriptive, not a fixed historical timing to reuse. Every future candidate must be paired with a fresh run of the current stable on these same six requests. Whole-suite time is about85 seconds on this host; correctness and additional driver diagnostics are separate from this release gate.


## Native transport control (2026-10-22)

The Windows-host HTTP origin remains the original v2 environment. Its timings
include the Docker/Windows forwarding path; do not count moving the origin as a
River speedup. The separately qualified native control serves byte-identical
copies of the same three fixtures from a read-only Docker volume,
`river-goal-fixed-native-data`. Each copy was hashed during transfer and read
back independently. Evidence: `reports/fixed-gate-v2/native-materialized.json`.
The generated fixtures and this volume never enter the deployment image.

`tools/serve-fixed-native-origin.py` exposes only the six manifest cases, with
version headers, size/mtime checks, real source-byte/request counters and trace
records. The materialization and manifest hashes accompany its control response.
The host-side runner still verifies originals, references and exact pixels.
Use `--external-origin` for the Docker-bridge URL and
`--external-origin-control` for the loopback control URL. Read the current
addresses from `reports/fixed-gate-v2/native-origin-volume-address.json`; container
addresses can change. OS caches are not flushed. This control is not measured
organizational S3 and does not establish Kakadu parity.

A/A qualification is `native-volume-same264-matched-control.json`: four balanced
rounds with identical image IDs, complete container environments and resource
limits, all three sizes, previews/deep tiles, and zero-source-IO exact repeats.
The preceding `native-volume-same264-control.json` has a configuration mismatch
annotation and must not be used as the clean control. Do not overwrite either.
The original source manifest and prior release evidence remain unchanged.

The runner now rejects unequal environments or resource limits for code/A/A
comparisons. Use `--comparison config` only for an explicitly labelled settings
experiment on the same current-stable image. Exact repeats fail the gate if any
source IO occurs. Source registration must remain IO-free. Retain the native data
volume and verification evidence under the Docker retention policy.


## Capacity origin and phase qualification

### Opt-in source CPU diagnostics

`RIVER_PROFILE_SOURCE=1` adds source preparation stage logs, including total
preparation across prefix/window fast paths. On Linux, `caller_cpu_ms` uses the
current thread's scheduler runtime and `cgroup_cpu_ms` uses container CPU usage.
The normal serving path does not read these counters. Unsupported counters are
reported as -1. Total includes the named subphases: do not add total to them.

Run `tools/profile-source-cpu.py` against an isolated, dedicated profiling
container and the qualified native origin. It checks all three sizes, reference
pixels, fresh source identities and zero-source-IO exact repeats. Container CPU
includes runtime workers and nginx; concurrent requests invalidate attribution.
These instrumented runs diagnose work versus waiting and are not release timing
comparisons. Phase CPU is not a network-only wait measurement, and total source
preparation excludes initial source/header resolution and decoding.

Local-file comparisons must use a fresh writable container layer for every cold
sample. Restarting an existing process is insufficient: deterministic local asset
identities can reuse persisted encoded tiles or source indexes. Reject a cold
response reporting an encoded-raster cache hit. Keep the original fixtures mounted
read-only and use identical resource limits, environment and catalog for both
images. `tools/benchmark-local-fixed.py` applies this rule across all three frozen
sizes in four balanced rounds. A failed request, including an OOM, has no latency
and must never enter timing medians. Report the memory ceiling with such failures.
Exact repeat bodies and server source-read telemetry are checked separately;
these checks do not constitute an independent OS-level file-read audit.

The interrupted `m270-local-paired.json` run is explicitly invalid for cold timing
because it reused encoded disk entries. Its replacement is
`m270-local-fresh-paired.json`, with per-sample fresh containers. An infrastructure
port-bind failure interrupted that replacement after 35 samples; a recorded resume
completed the missing samples without repeating or replacing earlier measurements.

Capacity timings must separate concurrent cold requests from concurrent exact
repeats. Decode/reference comparisons run after the cold timer stops and before
the repeat phase starts. Record server timing headers, cold/repeat batch medians,
real source IO, actual image IDs, and cgroup memory peaks. Preserve earlier mixed
cold/repeat/verification results as diagnostics rather than cold throughput gates.

`--shared-placement` runs both images in the same fresh, labelled test-container
slot, asserting an identical Docker-bridge address on every cohort. Reference
containers supply pinned images and identical environments; they are not the
measured slot. Test containers must have no data mounts or custom resource limits.
The owned slot is removed without deleting volumes after every cohort.

The native fixture origin requires an adequate listen backlog (256), records its
TCP_NODELAY setting, and reports namespace TCP counters. A capacity run fails if
its origin's ListenDrops or ListenOverflows increase. The previous default backlog
of five dropped connections during bursts: one recorded diagnostic had 578 drops.
Evidence is `reports/fixed-gate-v2/capacity-origin-overflow-diagnostic.json`.
Do not attribute changing the origin/network setup to a River optimization. The
single-request Windows-origin suite remains a separate primary comparison.

## Automatic concurrency qualification at checkpoint278

Canonical benchmark instances now leave RIVER_S3_RANGE_CONCURRENCY empty, matching the portable default. Record explicit limits separately: the prior four-lane setting can change speculative-index behavior. Checkpoint278 was compared directly with stable277 and LTS275 under identical automatic-concurrency environments, across all six frozen cases. Its distant-source gains use20ms injected response delay and adaptive read policy; no nearby or cached timing gain is claimed. A static UI retry input was added after timing, with an independently verified identical backend binary; m278-ui-repack-proof.json records measured and final image IDs.

## Connection and persisted-settings controls (protocol3)

ColdMs and repeatMs retain their existing fresh-connection timers. New repeatKeepAlive measurements use one untimed cached response to prime a new connection, then time one response on that same verified TCP connection. Source counters cover ALL phases, including priming; pixels and encoded repeat bytes must match. Reports identify the connection lifecycle explicitly. Never compare keepalive-repeat times with historical fresh-connection repeat times as a backend speedup.

Code and A/A comparisons require equal public runtime read-policy and encoding settings as well as equal environments/resources. The runner checks settings before the suite and after every restart, records only public tuning values, and rejects mismatches before timing. It never fetches whole configuration documents containing credentials. Explicit configuration comparisons retain separate settings and cannot be labelled identical-configuration controls.


## Channel lifecycle (user correction, 2026-11-02)

Latest is the current experiment. Stable is any verified improvement that is stable. LTS advances on each verified concrete >=10% improvement in a substantial workload. Channels may share an image. Advance aliases, release record, suite metadata, retention pins and current deployment metadata together; retain historical reports unchanged. Retire superseded images promptly instead of preserving old channels indefinitely. Keep about three distinct channel versions, at most four including immediate rollback, except explicitly reported external container references. Preserve volumes, cache, exported bundles and evidence.


## Opt-in source-read diagnostic

Use `tools/benchmark-controlled-cold.py --smoke-only --capture-source-reads` to collect a bounded fresh-process read trace for every frozen correctness case. The harness enables tracing before registration/render, clears its ring, verifies physical sends against the origin GET counter, rejects evicted traces, and disables tracing afterward (including an exit cleanup). These runs are diagnostic only and cannot supply paired release timing evidence. Normal timing runs keep tracing off. `responseBytes` records complete cold encoded response length; exact repeats must match the body and add no source I/O. Source traffic and client response traffic are separate metrics.

## Checkpoint789 qualification (2026-11-02)

Stable/LTS789 advances from744 on the frozen ten-layer TLM/PLT compact preview at target14ms RTT and300MB/s:222to184ms median, all six pairs faster, identical281600sourcebytes and12to9GETs. The16ms confirmation measured240to203ms;7ms measured164to150ms with one slower pair. These are shaped native-origin results, not actual organizationalS3. All six unchanged three-size requests passed six paired rounds, exact pixels and zeroIOrepeats; noisy primary medians do not establish broad latency gains. Single-layer probe behavior is unchanged. Network learning is shared by configured sourceID across images and is process-local; header-size learning in789 is image-local. See reports/kakadu-io-m303/header-read-ahead-release-m800.json for raw hashed evidence and limitations.

## Checkpoint811 qualification (2026-11-02)

Stable/LTS811 advances from789 on the unchanged979MBunindexed cold preview at16ms/300MB/s:1977to1631ms, all six pairs faster. Independent14ms:1702to1481ms, all six faster.7ms:1064to904ms with one slower pair. Six paired rounds cover all six unchanged three-size requests with exact pixels and zeroIOrepeats. Initial55MBdeep slowdown was investigated with Observe and independent Adaptive runs; it did not reproduce consistently, and all raw data remains attached. No universal small/deep/11GBgain is claimed. A slow-source guard and shared-source four-client correctness/admission screen passed; upgrade preserved settings,S3connections and volumes. Standard startup remains Observe; read ORGANIZATIONAL-S3-PROFILE.md for intended Adaptive settings. See reports/kakadu-io-m303/learned-seeds-release-m821.json for hashed evidence and limitations.

## Checkpoint830 Stable qualification (2026-11-02)

Stable830advances from811 on learned-source cold-object performance: deep tile1095to1023ms in twelve balanced pairs, all faster with identical188GETs/15905792bytes and exact pixels. Sources learn naturally across requests; this test first requests two other frozen images on the same sourceID. The confirmed gain is below10percent,so LTS remains811. Fresh sources retain full calibration; contradictory opening timing also retains it. Probe acceptance refreshes latency after the mandatory bandwidth pilot, preventing stale startup timing from authorizing16MiBbulk. All six fresh-source cases passed six paired rounds and zeroIOrepeats; real transitions and four-client learned-source admission checks passed. Final progressive viewport composites matched independent references across the same six cases; their sampled frame timings and intermediate preview pixels are not qualified as a speedup/exact-preview result. Standard startup remains Observe; read ORGANIZATIONAL-S3-PROFILE.md. Exact source provenance and raw limitations are in reports/kakadu-io-m303/source-refresh-release-m839.json; the mixed root worktree is preserved.

Benchmark sizing correction (refinement experiments): use `RIVER_VIEWPORT_DECODE_CONCURRENCY`, not the unrecognized `RIVER_PER_VIEWPORT_DECODE_CONCURRENCY`. Controlled runners reject that misspelling and record resolved startup sizing after each restart. Reports before this correction retain their raw environment and measured behavior: the reviewed907/934runs were matched at4effective jobs per viewport despite an ignored value1. Do not reinterpret those as single-job measurements. Preserve original results and publish annotations separately.

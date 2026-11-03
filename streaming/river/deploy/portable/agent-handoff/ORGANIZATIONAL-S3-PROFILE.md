# Organizational S3 performance profile

Read AGENTS.md and ORGANIZATIONAL-DEPLOYMENT.txt first. Read release.json and
PERFORMANCE.json for the exact image's qualification. A Latest candidate remains
experimental until those records explicitly qualify it.

The target described by the user is 300 **megabytes/second**, with7–14ms per
round-trip wave (also tested at16ms). The lab emulates this with a read-only
Linux HTTP Range origin and a shared300,000,000byte/s cap. It is not the actual
organizational S3 service. Source OS cache is warm; physical disk-cold timing,
production TLS/authentication and a32CPU optimum are not established here.

## Activate and verify the intended policy

The standard bundle's startup fallback remains Observe. Persisted API settings
override that fallback. Installing a new image does not itself enable adaptive
planning. GET /api/settings/read-policy and inspect settings.mode.

For an existing deployment, first retain the complete current settings object
and change only mode to adaptive. Preserve custom limits and all S3 connections.
The exact lab comparison used these deliberately selected values:

- mode: adaptive
- minimumSamples:8
- maxSourceRequests:8
- maxOverfetchRatio:2
- serialTransport:sdk
- refreshPayloadCosts:false

maxSourceRequests limits concurrent source operations per configured sourceID
per instance. It is not a GET-per-image cap or requests/second. Existing explicit
overfetch and request-byte limits still apply. Keep shard/lane overrides unset
for the automatic planner; preserve and record existing custom overrides before
testing alternatives. The lab used16CPU/8GiB, OpenJPEG, and a64MiB source-byte
cache; these are measurement conditions, not deployment sizing instructions.

For a reviewed merge of the full lab policy, the existing helper prints its
proposal without writing:

```sh
python3 apply-nearby-read-policy.py --api http://127.0.0.1:8080 --transport sdk --source-requests 8
```

Its historical filename says nearby, but the proposed fields above are also
the shaped organizational-network baseline. Review its before/proposed values.
If testing that full profile is intended, add --apply --record with a new private
record path. It GETs and merges only read-policy settings, preserves indexHeaders
and other fields, then reads the effective settings back. It does not replace S3
connections or import a whole configuration. Verify effective values after a
restart. Preserve a lower existing source cap for the initial baseline.

SDK is the tested transport here. Auto's guarded native serial transport is a
separate near-source optimization; at these sustained latencies it retains SDK.

## What learns at source scope

Network latency, throughput observations and source admission are shared across
requests and images under the same configured sourceID. Different sourceIDs learn
independently even at the same endpoint. Measurements are process-local and reset
on restart, connection replacement/deletion or whole-configuration import.
Observe measures without applying adaptive merges; Disabled stops learning.
Exact rendered-cache repeats do not fetch source bytes or train network estimates.

Request plans also use each image's required ranges, tile-part sizes and index
availability. More source bytes can be worthwhile when they eliminate latency
waves; slow throughput can favor more smaller requests. Limits and pixel
validation remain mandatory. A GET count alone does not measure dependent waves.

Checkpoint811's seed-geometry improvement changes counts only for eligible whole-image
previews, with measured latency at least5ms and sufficient observed nontrivial
part sizes. It retains the16MiB speculative probe ceiling and existing bandwidth
guard. Partial views retain their prior automatic count; explicit overrides win.
Header-size learning in789 is image-local, separate from shared network learning.

Starting with830, a mature source can reuse its discovery confidence after at least
three fresh preparation reads and three validated nontrivial parts on the new image.
The source needs a mature eight-header window, the configured minimum sample count,
and three measured body transfers. The opening read must agree with source history;
contradictory opening measurements retain the full current-image calibration. Fresh
sources retain the eight-read warmup. Geometry checks inspect only recent parts.
The mandatory bandwidth pilot remains, and its updated latency is checked before
committing bulk traffic. A short startup delay ending during the pilot therefore
does not authorize a large speculative read using stale distant-source timing.
This is process-local learning, not persisted network calibration across restarts.

## Verify the real deployment

Compare identical original images, viewports, driver, output and resource limits.
Keep fresh-source cold objects, learned-source cold objects, exact repeats,
progressive frame arrivals and browser paint timings separate. Record effective
read policy, source GET/HEAD counts and bytes, source/preparation versus decode and
encode timings, pixels, source admission pressure and the exact deployed imageID.
Keep the same sourceID across different images when checking accumulated learning.
Do not recreate connections or scan a bucket just to train the planner.

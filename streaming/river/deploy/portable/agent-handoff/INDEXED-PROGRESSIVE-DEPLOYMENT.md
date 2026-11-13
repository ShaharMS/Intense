# Indexed progressive delivery in m1199

Use PERFORMANCE.json to verify the image digest, the measured workload, and
the comparison baseline. The qualifying workload is a cold progressive
viewport on a 979 MB TLM-indexed JPEG 2000 fixture with 14 ms configured origin
delay and 300 MB/s throughput. Use the measured results for that workload.

River sends the smallest preview batch first. For eligible larger indexed
preview batches, it can read a bounded prefix for the subsequent pass while
using up to eight range lanes. The optimization requires adaptive read
policy, an observed source latency of at least 10 ms, typed TLM entries, and
a source cap of at least eight requests. An explicit
RIVER_S3_RANGE_CONCURRENCY setting takes precedence. Existing persisted API
settings and source credentials should be preserved during upgrade.

To disable only this bulk preview optimization, set
RIVER_PROGRESSIVE_BULK_PREVIEW_LANES=0 and restart the backend. The earlier
first-preview scheduling and final-refinement behavior continue to apply.

For a deployment check, compare the first progressive frame, full preview
coverage, and final frame separately. Record origin GETs, transferred bytes,
and pixel equality on the same image and viewport. A second request or a
zoom after preview may reuse cached data, so record cache state with each
measurement. Test the actual client origin over the configured TLS protocol.

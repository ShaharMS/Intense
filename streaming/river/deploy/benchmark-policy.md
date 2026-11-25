# River qualification policy

Stable, LTS and Latest are aliases for one qualified implementation and image.
This explicitly requested consolidation replaces the previous frozen-channel split;
it does not assert a new 10% performance gain. Candidates remain unqualified until
comparisons and correctness checks identify their exact image digest.

Generate the corpus locally with tools/fixtures/generate-corpus.py, then verify
marker layouts. Preserve reference pixels independently of the decoder being tested.
Use matched CPU/memory limits, image hashes, driver, output format, viewport and
origin conditions. Report actual decoder and fallbacks. Distinguish new-source
cold, metadata-warm, exact repeats, progressive frame arrival and browser paint.
Process startup, manifest preparation and warmed OS page cache must be disclosed.

Measure source GETs, HEADs and actual bytes at the controlled origin, preserving
range traces. Response counters can describe only a suboperation and are not a
substitute for wire measurements. Never equate requested driver with actual decoder.

Use at least three paired alternating repetitions for latency comparisons. Report
per-image/layout values and regressions as well as aggregates. Synthetic fixtures
are controlled evidence, not proof for every production codestream. Preserve the
raw report under evidence/ when qualifying; generated runs live under target/.

Cache redistribution needs a sustained workload spanning its sampling/hysteresis
windows. Check budget conservation, explicit limits, live disable, memory pressure
and recovery. Larger cache budgets alone do not establish performance benefit.

Maintain the existing river Compose project and at most six named backend slots.
Recreate slots without deleting volumes; do not create one container per test.
Retire superseded images after checking references. Historical archives/evidence
can remain available without keeping their Docker images installed.

Cache allocation must use measured producer costs reused, with coverage, alongside
reuse and eviction pressure. Unavailable measurements must be explicit and must
not be treated as zero benefit. Nested saved-work estimates are not additive.
Source feedback must compare observed origin bytes against unique demanded bytes,
exclude failed/retried/incomplete samples, obey configured ceilings and warmup,
and recover slowly. A lower optional allowance is not evidence of fewer wire bytes.

# Optional near-source serial transport profile

This uses the unchanged 744 backend. It changes only the serial transport setting.
Two six-pair local comparisons on the frozen 979 MB preview measured 431 to 345 ms
and 411 to 366 ms. Every pair improved, pixels matched, and traffic remained
349 GETs / 3,134,690 bytes. These are local native-HTTP-origin results with warm
origin OS pages, four CPUs, one decode slot, four decoder workers, source cap four,
and adaptive reads with eight samples and a 2x overfetch limit.

Auto starts with SDK reads. It can select native transport after sustained,
isolated serial discovery on a suitable fast source. Indexed/small sources and
contended preparations retain SDK. The tested four-client cohorts respected the
shared source limit and used SDK under contention. Unsupported local native
initialization also fell back to SDK with exact pixels and no added requests.

At organizational RTTs of 7–16 ms, this selector retains SDK. Qualification of
request waves at those latencies and 300 MB/s is a separate workload.

## Existing installations

Keep the existing image configuration, `.env`, S3 IDs, credentials, volumes and
read-policy limits. Review a transport-only change from inside the pod or host:

```sh
python3 apply-serial-transport.py --api http://127.0.0.1:8080 --transport auto
python3 apply-serial-transport.py --api http://127.0.0.1:8080 --transport auto --apply --record serial-before.json
```

The first command only displays the proposed change. The second persists it and
reads the settings back. Confirm a known existing image and the source transport
counters. Saved API settings override environment fallbacks after restart.

Restore only the recorded transport while preserving later tuning changes:

```sh
python3 apply-serial-transport.py --api http://127.0.0.1:8080 --restore serial-before.json --apply --record serial-restore.json
```

The record contains public tuning values, not source credentials. This helper
does not read or replace whole configuration documents or S3 definitions.

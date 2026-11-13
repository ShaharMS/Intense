"""Container sizing defaults; explicit environment settings always win."""
import os, json, subprocess
from pathlib import Path
PROFILES = {
    'compact': {},
    'throughput16': {
        'RIVER_DECODE_CONCURRENCY': '8',
        'RIVER_SOURCE_IO_CONCURRENCY': '16',
        'RIVER_OPENJPEG_THREADS_PER_DECODE': '1',
        'RIVER_GROK_THREADS_PER_DECODE': '2',
        # Native Grok owns one process-wide pool, shared by admitted decodes.
        'RIVER_GROK_NATIVE_THREADS': '8',
        'RIVER_S3_RANGE_CACHE_BYTES': '268435456',
        'RIVER_RASTER_CACHE_BYTES': '268435456',
    },
}

def apply_profile(environ):
    name = environ.get('RIVER_PROFILE', 'auto')
    if name not in (*PROFILES, 'auto'):
        raise ValueError('RIVER_PROFILE must be auto, compact or throughput16')
    defaults = dict(PROFILES.get(name, {}))
    # The Rust resource coordinator owns automatic sizing and diagnostics.
    defaults.update(RIVER_GROK_NATIVE='true', RIVER_WEBP_METHOD='0')
    # OpenJPEG constructs its initial pool before River applies each codec's
    # explicit thread budget. Mirroring the request limit here creates and
    # destroys a pool for every small native tile. Start synchronously; the
    # per-codec setting still enables workers for substantial decode jobs.
    defaults['OPJ_NUM_THREADS']='0'
    for key, value in defaults.items():
        environ.setdefault(key, value)
    # GPU helper workers use the same detected budget; explicit settings win.
    server=Path('/usr/local/bin/river-server')
    if server.exists():
        values=json.loads(subprocess.check_output([str(server),'--resource-env'],env=dict(environ),text=True))
        for key,value in values.items():
            if environ.get(key, "").strip().lower() in ("", "auto"): environ[key]=value
    return name

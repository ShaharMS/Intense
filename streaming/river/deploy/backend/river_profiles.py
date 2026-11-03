"""Container sizing defaults; explicit environment settings always win."""
import os
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
    if name == 'auto':
        cpus = len(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else os.cpu_count() or 1
        memory = 2 * 1024**3
        try:
            memory = os.sysconf('SC_PAGE_SIZE') * os.sysconf('SC_PHYS_PAGES')
        except (AttributeError, ValueError, OSError):
            pass
        try:
            quota, period = Path('/sys/fs/cgroup/cpu.max').read_text().split()
            if quota != 'max': cpus = min(cpus, max(1, (int(quota)+int(period)-1)//int(period)))
        except (OSError, ValueError):
            pass
        try:
            limit = Path('/sys/fs/cgroup/memory.max').read_text().strip()
            if limit != 'max': memory = min(memory, int(limit))
        except (OSError, ValueError):
            pass
        slots = max(1, min(8, cpus//2, memory//(2*1024**3)))
        cache = max(16*1024**2, min(256*1024**2, memory//32))
        defaults.update(RIVER_DECODE_CONCURRENCY=str(slots), RIVER_SOURCE_IO_CONCURRENCY=str(min(8,max(2,slots*2))),
                        RIVER_RASTER_CACHE_BYTES=str(cache), RIVER_S3_RANGE_CACHE_BYTES=str(cache))
    defaults.update(RIVER_GROK_NATIVE='true', RIVER_WEBP_METHOD='0')
    # OpenJPEG constructs its initial pool before River applies each codec's
    # explicit thread budget. Mirroring the request limit here creates and
    # destroys a pool for every small native tile. Start synchronously; the
    # per-codec setting still enables workers for substantial decode jobs.
    defaults['OPJ_NUM_THREADS']='0'
    for key, value in defaults.items():
        environ.setdefault(key, value)
    return name

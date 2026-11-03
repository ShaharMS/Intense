"""Sizing choices, explicit overrides, and invalid-profile rejection."""
import importlib.util
from pathlib import Path

path = Path(__file__).resolve().parents[1] / 'deploy/backend/river_profiles.py'
spec = importlib.util.spec_from_file_location('river_profiles', path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
compact = {'RIVER_PROFILE':'compact'}
assert module.apply_profile(compact) == 'compact' and 'RIVER_DECODE_CONCURRENCY' not in compact
auto = {'RIVER_DECODE_CONCURRENCY':'3','RIVER_WEBP_METHOD':'4'}
assert module.apply_profile(auto) == 'auto'
assert auto['RIVER_DECODE_CONCURRENCY']=='3' and auto['RIVER_WEBP_METHOD']=='4'
assert 2 <= int(auto['RIVER_SOURCE_IO_CONCURRENCY']) <= 8
from unittest.mock import patch
class Cgroup:
    def __init__(self, path): self.path = path
    def read_text(self): return '200000 100000' if self.path.endswith('cpu.max') else str(2*1024**3)
with patch.object(module,'Path',Cgroup), patch.object(module.os,'cpu_count',return_value=16):
    constrained={}
    module.apply_profile(constrained)
    assert constrained['RIVER_DECODE_CONCURRENCY']=='1'
    assert constrained['RIVER_SOURCE_IO_CONCURRENCY']=='2'
settings = {'RIVER_PROFILE': 'throughput16', 'RIVER_DECODE_CONCURRENCY': '4'}
assert module.apply_profile(settings) == 'throughput16'
assert settings['RIVER_DECODE_CONCURRENCY'] == '4'
assert settings['RIVER_SOURCE_IO_CONCURRENCY'] == '16'
assert settings['RIVER_S3_RANGE_CACHE_BYTES'] == '268435456'
assert settings['RIVER_GROK_NATIVE_THREADS'] == '8'
assert settings['OPJ_NUM_THREADS']=='0'
roi_workers={'RIVER_OPENJPEG_THREADS_PER_DECODE':'3'}
module.apply_profile(roi_workers)
assert roi_workers['OPJ_NUM_THREADS']=='3'
explicit_roi={'OPJ_NUM_THREADS':'2','RIVER_OPENJPEG_THREADS_PER_DECODE':'4'}
module.apply_profile(explicit_roi)
assert explicit_roi['OPJ_NUM_THREADS']=='2'
explicit_native={'RIVER_PROFILE':'throughput16','RIVER_GROK_NATIVE_THREADS':'3'}
module.apply_profile(explicit_native)
assert explicit_native['RIVER_GROK_NATIVE_THREADS']=='3'
explicit_io={'RIVER_PROFILE':'throughput16','RIVER_SOURCE_IO_CONCURRENCY':'6'}
module.apply_profile(explicit_io)
assert explicit_io['RIVER_SOURCE_IO_CONCURRENCY']=='6'
try:
    module.apply_profile({'RIVER_PROFILE': 'unknown'})
except ValueError:
    pass
else:
    raise AssertionError('unknown sizing profile accepted')
print('PASS: compact defaults, throughput sizing, explicit overrides, invalid-profile rejection')

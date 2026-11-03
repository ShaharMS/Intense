import importlib.util,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));h=importlib.util.module_from_spec(spec);spec.loader.exec_module(h)
values=dict(decode_concurrency=4,per_viewport_decode_concurrency=4,openjpeg_threads_per_decode=16,max_native_tiles_per_job=4,preview_extra_reductions=2,refine_idle_delay_ms=150)
def event(t=100,e=1,omit=None):
 lines=[f't={t} INFO e={e} r=0 s=0 message=configured JPEG 2000 decode concurrency']
 for k,v in values.items():
  if k!=omit:lines.append(f't={t} INFO e={e} r=0 s=0 {k}={v}')
 return lines
class Sizing(unittest.TestCase):
 def test_split_event_ignores_interleaving(self):
  lines=event();lines.insert(2,'t=100 INFO e=2 r=0 s=0 openjpeg_threads_per_decode=99');self.assertEqual(h.parse_effective_sizing('\n'.join(lines)),values)
 def test_latest_incomplete_restart_cannot_reuse_old_values(self):
  self.assertIsNone(h.parse_effective_sizing('\n'.join(event()+event(200,1,'openjpeg_threads_per_decode'))))
 def test_legacy_single_line(self):
  self.assertEqual(h.parse_effective_sizing('configured JPEG 2000 decode concurrency '+' '.join(f'{k}={v}' for k,v in values.items())),values)
 def test_conflicting_field_rejected(self):
  self.assertIsNone(h.parse_effective_sizing('\n'.join(event()+['t=100 INFO e=1 r=0 s=0 openjpeg_threads_per_decode=99'])))
if __name__=='__main__':unittest.main()

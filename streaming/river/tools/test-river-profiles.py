"""Regression checks for global OpenJPEG initialization versus per-codec budgets."""
import importlib.util,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('profiles',Path(__file__).resolve().parents[1]/'deploy/backend/river_profiles.py')
profiles=importlib.util.module_from_spec(spec);spec.loader.exec_module(profiles)
class Profiles(unittest.TestCase):
    def test_large_request_budgets_do_not_create_a_pool_in_every_new_codec(self):
        for name in ['auto','compact','throughput16']:
            for workers in [2,8,16]:
                env=dict(RIVER_PROFILE=name,RIVER_OPENJPEG_THREADS_PER_DECODE=str(workers))
                profiles.apply_profile(env)
                self.assertEqual(env['OPJ_NUM_THREADS'],'0')
                self.assertEqual(env['RIVER_OPENJPEG_THREADS_PER_DECODE'],str(workers))
    def test_explicit_global_openjpeg_setting_is_preserved(self):
        env=dict(RIVER_PROFILE='compact',RIVER_OPENJPEG_THREADS_PER_DECODE='16',OPJ_NUM_THREADS='3')
        profiles.apply_profile(env)
        self.assertEqual(env['OPJ_NUM_THREADS'],'3')
    def test_default_initialization_remains_synchronous(self):
        for name in ['auto','compact','throughput16']:
            env=dict(RIVER_PROFILE=name);profiles.apply_profile(env)
            self.assertEqual(env['OPJ_NUM_THREADS'],'0')
if __name__=='__main__':unittest.main()

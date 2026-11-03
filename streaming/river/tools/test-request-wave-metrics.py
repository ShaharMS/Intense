import unittest
from request_wave_metrics import audit_requests
def rows(spans):return [dict(body=True,start=a,end=b) for a,b in spans]
class Intervals(unittest.TestCase):
    def test_parallel_reads_share_one_round(self):
        r=audit_requests(rows([(0,.007)]*4));self.assertEqual((r['requests'],r['maximumNonOverlappingRequests'],r['peak']),(4,1,4))
    def test_serial_reads_keep_cpu_gaps_separate(self):
        r=audit_requests(rows([(0,.007),(.008,.015),(.016,.023)]));self.assertEqual(r['maximumNonOverlappingRequests'],3);self.assertAlmostEqual(r['ioBusyMs'],21);self.assertAlmostEqual(r['idleGapMs'],2)
    def test_staggered_pipeline_is_not_one_round(self):
        r=audit_requests(rows([(i/1000,(i+4)/1000) for i in range(8)]));self.assertEqual(r['maximumNonOverlappingRequests'],2);self.assertEqual(r['peak'],4)
    def test_last_started_need_not_finish_last(self):
        self.assertEqual(audit_requests(rows([(0,.010),(.001,.002)]))['spanMs'],10)
    def test_cache_hit_has_no_request_interval(self):
        self.assertEqual(audit_requests([])['requests'],0)
if __name__=='__main__':unittest.main()

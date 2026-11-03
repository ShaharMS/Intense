"""Compare encoded compact previews against checkpoint 69, not just dimensions."""
import argparse,importlib.util,json,sys
from pathlib import Path
from urllib.request import urlopen
p=argparse.ArgumentParser();p.add_argument('--old-api',required=True);p.add_argument('--new-api',required=True)
p.add_argument('--range-latency-ms',type=float,default=0);p.add_argument('--report-prefix',default='reuse-encoded-equivalence');a=p.parse_args()
root=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('benchmark',root/'tools/benchmark-s3-corpus.py');module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
module.WS=module.WS.replace('const cfg =',"const {createHash}=process.getBuiltinModule('node:crypto');\nconst cfg =").replace('frames.push({ x:',"frames.push({ payloadSha256:createHash('sha256').update(bytes.subarray(8+size)).digest('hex'), x:")
module.WS="(async()=>{\n"+module.WS+"\n})().catch(error=>{console.error(error);process.exit(1);});"
payloads={}
for label,api in [('old',a.old_api),('new',a.new_api)]:
    assert json.load(urlopen(api+'/api/settings/encoding'))['webpMethod']==0
    path=root/'reports'/f'{a.report_prefix}-{label}.json'
    sys.argv=['benchmark','--api',api,'--driver','jp2grok','--objects','large','--progressive-patches','--repeat-runs','1','--range-latency-ms',str(a.range_latency_ms),'--output',str(path)]
    assert module.main()==0
    frames=json.loads(path.read_text())['measurements'][1]['frames']
    payloads[label]={(frame['x'],frame['y']):frame['payloadSha256'] for frame in frames}
assert payloads['old']==payloads['new'],payloads
print('PASS: compact WebP preview payloads are byte-identical to checkpoint 69; no lossy patch recompression')

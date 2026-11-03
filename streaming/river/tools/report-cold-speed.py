import json, math, statistics
from pathlib import Path

root = Path(__file__).resolve().parents[1]
d = json.loads((root/'reports/wmts-drivers-2026-10-11T12-00-55.508Z.json').read_text())
def stats(v):
    v=sorted(v)
    return [len(v), round(statistics.mean(v),2), round(statistics.median(v),2), round(v[math.ceil(.95*len(v))-1],2),round(v[math.ceil(.99*len(v))-1],2)]
groups={}
for s in d['samples']:
    if s['ok'] and isinstance(s.get('server'),(int,float)) and not s['group'].endswith('warmup'):
        groups.setdefault(s['group'],[]).append(s)
rows=[[g,*stats([s['server'] for s in ss])] for g,ss in groups.items()]
reductions=[]
for g,ss in groups.items():
    if g.startswith('nvjpeg2000') and g.endswith('pinpoint'):
        for matrix in (0,4,6):
            v=[s['server'] for s in ss if f'&TILEMATRIX={matrix}&' in s['url']]
            if v: reductions.append([g,6-matrix,*stats(v)])
spans={}
for key,s in d['spans'].items():
    g,name=key.split('|',1)
    if g.endswith('warmup'): continue
    spans.setdefault(name,[]).append([g,s['n'],*[round(s[k],2) for k in ('mean','median','p95','p99')]])
intro='''# River cold-path and 50 ms target investigation

Large mosaic: 40980 × 10980, 658123436-byte JPEG2000, 451 native tiles. Measurements are backend x-river-server-ms; admission queue is separately reported and its median is about 0.1 ms. Warm pinpoint has 18 requests per format (three positions at reductions 0, 2, 6); bursts have 32 full-resolution requests at concurrency 8 or 32. No decoded-raster/result cache. Percentiles use nearest rank; small n means p95/p99 often equal maximum. Timing run: 2026-10-11T12:00:55Z, Windows client to local Docker, no competing build.

## Why cold requests took seconds

Two independent causes were reproduced. Synchronous source copying/hash verification cost 4901.5 ms server time and held a global cache lock, stalling unrelated already-ready sources by about 5.8 seconds. Staging now happens in a bounded background worker with nonblocking ready lookup; the corresponding source-cold Grok overview measured 222.5 ms and ready requests during another fill measured 56–84 ms server time.

A different untouched GPU overview still measured 7615.1 ms server time, including 7429.4 ms native decode. The unindexed RPCL source forced expensive high-resolution packet access for a tiny overview. The new exact compressed lowest-resolution projection reads 3698304 bytes and creates a 334016-byte codestream rather than decoding the original 658123436-byte source. It retains native GPU decode and the source's lossless reduced pixels. Strictly unsupported layouts fall back to normal decode. This optimization currently targets the coarsest resolution of the catalog layout.

The same GPU asset, with empty source/view caches and a warmed healthy process, measured 332.9 ms server / 353.5 ms client on its first request, versus 7615.1 / 7694.8 ms before (about 23×). Subsequent server samples ranged 82–114 ms. OS disk caches were not deliberately dropped; this is not a controlled physical-disk-cold comparison. Startup health explicitly reports worker warmup (171.7 ms in the recorded run).

## 50 ms target

Sequential native full/intermediate-resolution GPU tile medians meet 50 ms. Coarsest overviews and burst loads do not. The mixed pinpoint median hides the overview cost, so the per-resolution table is the relevant view. Under concurrency 32, sidecar HTTP median is 163.8 ms for WebP while encode is 12.0 ms; more efficient admission and GPU/CPU pipeline scheduling are the next targets. Coarse overviews still require hundreds of small tile submissions. Future ingestion with a precinct-oriented layout/fewer native tiles should be evaluated separately, with full-resolution tradeoffs measured.

## Research and ablations

Independent CUDA streams reduced native overview median from 301.7 ms (one stream) to 169.2 ms (four); eight did not improve it. Exact decode-area ROI reduced mosaic full-resolution native mean from 18.60 to 10.42 ms. A lossless TLM marker experiment passed 128 pixel hashes but produced no meaningful local-I/O speedup, so it was not enabled. Grok's marker transcode crashed on this input; no rewritten source was adopted. No original imagery was modified.

References: [NVIDIA API](https://docs.nvidia.com/cuda/nvjpeg2000/apireference.html), [NVIDIA stream strategy](https://developer.nvidia.com/blog/accelerating-jpeg-2000-decoding-for-digital-pathology-and-satellite-images-using-the-nvjpeg2000-library/), [IIPImage source-layout guidance](https://iipimage.sourceforge.io/documentation/images), [Grok](https://github.com/GrokImageCompression/grok).

## Correctness and validation

35/35 full reduced catalog images exactly match normalized 16-bit Grok reference pixels, maximum channel error 0. HTTP validation: 108/108 same-format probes exact, no failures; another 96 mixed GPU requests at concurrency 32 match CPU references. Final Rust suite: 13 passed. The final reproducible Docker images were rebuilt and started locally. Strict unsupported codestreams retain the general decoder path and can retain cold costs.

## Backend latency by workload (ms)
'''
headers=['Workload','n','Average ms','Median ms','p95 ms','p99 ms']
def table(h,r): return '| '+' | '.join(h)+' |\n| '+' | '.join(['---']*len(h))+' |\n'+''.join('| '+' | '.join(map(str,x))+' |\n' for x in r)
out=intro+table(headers,rows)+'\n## GPU backend latency by reduction (ms)\n'+table(['Workload','Reduction',*headers[1:]],reductions)
for name,r in spans.items(): out+='\n## '+name+' (inclusive span ms)\n'+table(headers,r)
out+='\nNested trace spans overlap and must not be summed. Background cache fill is separately linked and does not extend foreground request spans. Raw measurements: wmts-drivers-2026-10-11T12-00-55.508Z.json; cold-source-before.json; cold-source-after.json; host-cold-gpu.json; cold-gpu-warmed-process-after.json; resolution-views-full-corpus.json.\n'
(root/'reports/cold-speed-results.md').write_text(out,encoding='utf-8')
canvas=Path('C:/Users/shaha/.cursor/projects/c-Users-shaha-Documents-GitHub-Intense/canvases/WMTS-cold-path.canvas.tsx')
canvas.write_text("import {H1,H2,Text,Table,Stack} from 'cursor/canvas';\nconst rows="+json.dumps(rows)+";\nconst reductions="+json.dumps(reductions)+";\nconst spans="+json.dumps(spans)+";\nexport default function Report(){return <Stack gap={18}><H1>River: cold latency and the 50 ms target</H1><Text>40980 × 10980 mosaic · backend milliseconds · 2026-10-11 12:00 UTC. Sequential fine-resolution tiles meet the target; coarse overviews and bursts remain above it.</Text><H2>Cold overview: same GPU asset</H2><Table headers={['Measurement','Before ms','After ms']} rows={[[ 'Server first request',7615.1,332.9],['Client first request',7694.8,353.5]]}/><Text>Source and compact-view caches cold; healthy warmed process after. OS caches were not dropped. Async byte staging removes global blocking. An exact compressed LL projection reads 3.53 MiB of a 628 MiB source and creates a 326 KiB stream; no decoded result cache.</Text><H2>GPU backend by reduction</H2><Table headers={['Workload','Reduction','n','Average ms','Median ms','p95 ms','p99 ms']} rows={reductions}/><H2>All drivers and loads</H2><Table headers={['Workload','n','Average ms','Median ms','p95 ms','p99 ms']} rows={rows}/><Text>Pinpoint: 18 per format, three positions × three reductions. Bursts: 32 full-resolution requests at concurrency 8/32. Small samples make high percentiles equal maxima. Admission queue median about 0.1 ms; server includes sidecar waiting.</Text><H2>Pixel correctness</H2><Text>35/35 complete reduced catalog images exact, max channel delta 0. 108/108 HTTP format probes exact, 96 concurrent mixed GPU probes exact, zero failures. Rust: 13 passed. Unsupported compressed layouts use the normal decoder.</Text><H2>Remaining bottlenecks</H2><Text>Coarse overview still submits hundreds of native tiles. At concurrency 32 WebP, sidecar HTTP median 163.8 ms versus encode median 12.0 ms. Further work should target coarse tile scheduling and pipeline admission. TLM-only indexing did not materially improve local I/O.</Text>{Object.entries(spans).map(([name,r])=><Stack key={name} gap={8}><H2>{name}</H2><Table headers={['Workload','n','Average ms','Median ms','p95 ms','p99 ms']} rows={r}/></Stack>)}<Text>Trace spans are inclusive and overlap; do not sum. Source: reports/wmts-drivers-2026-10-11T12-00-55.508Z.json. Cold evidence and experiment details: reports/cold-speed-results.md.</Text></Stack>}\n",encoding='utf-8')
print('Wrote complete report and canvas')

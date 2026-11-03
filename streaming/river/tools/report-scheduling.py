import json,math,statistics
from pathlib import Path
root=Path(__file__).resolve().parents[1]
def read(name):return json.loads((root/'reports'/name).read_text(encoding='utf-8-sig'))
def fmt(x):return round(x,2) if isinstance(x,(int,float)) else '—'
def stats(v):
 v=sorted(v)
 return dict(n=len(v),mean=statistics.mean(v),median=statistics.median(v),p95=v[math.ceil(.95*len(v))-1],p99=v[math.ceil(.99*len(v))-1])
def table(headers,rows):return '| '+' | '.join(headers)+' |\n| '+' | '.join(['---']*len(headers))+' |\n'+''.join('| '+' | '.join(map(str,row))+' |\n' for row in rows)
headers=['Workload','n before / after','Mean before','Mean after','Median before','Median after','p95 before','p95 after','p99 before','p99 after']
def row(label,b,a):return [label,f"{b['n']} / {a['n']}",*[fmt(s[k]) for k in ['mean','median','p95','p99'] for s in [b,a]]]
vbefore=read('viewport-load-before-ablation.json');vafter=read('viewport-load-after.json')
viewport=[row(g,vbefore['summary'][g]['serverMs'],v['serverMs']) for g,v in vafter['summary'].items()]
datasets=[]
for size,baseline,log in [('40980 × 10980','wmts-drivers-2026-10-11T12-00-55.508Z.json','scheduling-final-mosaic.log'),('2592 × 1944','wmts-drivers-2026-10-11T09-06-26.686Z.json','scheduling-final-small.log')]:
 before=read(baseline);after_path=read(log)['path'];after=json.loads((root/after_path).read_text(encoding='utf-8-sig'))
 rows=[];e2e=[];reductions=[];spans={};stages={}
 for g,v in after['summary'].items():
  if not any(g.endswith(s) for s in ['pinpoint','burst8','burst32']):continue
  for key,target in [('server',rows),('e2e',e2e)]:target.append(row(g,before['summary'][g]['metrics'][key],v['metrics'][key]))
 for key,a in after['spans'].items():
  g,name=key.split('|',1)
  if not any(g.endswith(s) for s in ['pinpoint','burst8','burst32']):continue
  b=before['spans'].get(key)
  if b:spans.setdefault(name,[]).append(row(g,b,a))
 for format in ['png','webp']:
  g=f'nvjpeg2000/{format}/pinpoint'
  for reduction in [0,2,after['manifest']['resolutionLevels']-1]:
   populations=[]
   for d in [before,after]:
    level=d['manifest']['resolutionLevels']-1-reduction
    populations.append([s['server'] for s in d['samples'] if s['group']==g and s['ok'] and f'&TILEMATRIX={level}&' in s['url']])
   reductions.append(row(g+f'/r{reduction}',*[stats(v) for v in populations]))
 for stage in ['sidecar_queue_ms','parse_ms','native_decode_ms','raster_ms']:
  for g in after['summary']:
   if not g.startswith('nvjpeg2000') or not any(g.endswith(s) for s in ['pinpoint','burst8','burst32']):continue
   populations=[]
   for d in [before,after]:
    populations.append([float(t['value']) for r in d['traces'] if r['group']==g for s in r['trace']['spans'] if s['operationName']=='river.sidecar.http' for t in s['tags'] if t['key']==stage])
   if all(populations):stages.setdefault(stage,[]).append(row(g,*[stats(v) for v in populations]))
 datasets.append(dict(size=size,before=baseline,after=after_path,rows=rows,e2e=e2e,reductions=reductions,spans=spans,stages=stages,traceCount=len(after['traces']),errors=after['errors'],failed=sum(v['failed'] for v in after['summary'].values())))
correct=read('scheduling-pixel-correctness.json');mixed=read('scheduling-mixed-correctness.json');corpus=read('packed-final-corpus.json')['samples']
assert len(correct)==108 and all(s.get('sameFormatEqualPixels') for s in correct)
assert mixed['failed']==0 and len(mixed['samples'])==96
assert len(corpus)==35 and all(s['equalPixels'] and s['maxError']==0 for s in corpus)
spatial=read('scheduling-spatial-correctness.json')
assert len(spatial['samples'])==100 and spatial['failed']==0 and max(s['requests'] for s in spatial['batchMeasurements'])>1
assert all(not d['errors'] and d['failed']==0 for d in datasets)
intro='''# River: packed overviews and concurrent scheduling

Backend target: 50 ms median per tile on large imagery. **Met for coarse overviews at concurrency 8 and 32, and adjacent/mixed viewports at concurrency 8. Not met for adjacent/mixed loads at concurrency 32 or scattered full-resolution bursts.** No decoded image/result cache is used. First-use parse costs and high percentiles remain visible.

## Changes

The coarsest lossless JPEG2000 packets are regrouped, without decoding/re-encoding their entropy bytes, into one zero-DWT native GPU tile. Independent original codeblocks become precincts with identical active codeblock boundaries and raster/precinct/component ordering. The catalog overview changes from 451 GPU tile submissions to one. Strict eligibility checks retain the original compact multi-tile projection when packing is unsupported; unsupported projections use ordinary native decoding.

The sidecar uses eight persistent GPU workers with a bounded oldest-first admission queue. A one-millisecond microbatch window groups compatible overview requests or fine ROIs with identical native-tile footprints. Gathering workers reserve their group so other workers cannot split the same arriving batch. Unrelated scattered fine ROIs are not grouped: an ablation showed larger decode rectangles and worse latency. Each worker retains at most two parsed source streams, avoiding full-source/overview parse eviction. GPU and pinned buffers reserve 8 MiB per worker at startup; precision normalization uses exact integer lookup tables. PNG uses fast lossless compression and can produce larger responses.

## Adjacent and mixed viewport load: backend milliseconds

128 measured WebP requests per cell at concurrency 8/32, eight warmups excluded per workload. Four-by-two neighboring tile grids change origin every eight requests. Mixed requests cycle reductions 6, 2, 0. Overviews alternate the available low-resolution tiles. These are compute requests with no decoded result cache, including repeated coordinates. Admission and RPC waiting are included in server time; client/network/image-decode time is separate.

This paired ablation disables packing, batching, reservation and uses four workers for the before preset; after enables them with eight workers. Both use the current exact lookup normalization, two parsed-stream retention, identical WebP quality and encoder. It isolates those flags and is not a replay of the old binary. Dates/times and every request are retained in viewport-load-before-ablation.json and viewport-load-after.json. The original scattered workload below compares the previously recorded full baseline with the final implementation.

'''
report=intro+table(headers,viewport)
report+='''
## Correctness and limits

35/35 complete reduced catalog images match normalized 16-bit Grok references exactly, maximum channel delta zero. 108/108 HTTP format probes match their respective CPU format exactly; 96 mixed-source/depth/resolution PNG requests at concurrency 32 match CPU references, zero failures. Another 100 spatial probes at concurrency 32 cover neighboring tiles across two assets and reductions 0/2/max; every channel matches, and traces confirm batches of up to four requests. PNG is lossless; WebP pixel checks compare decoded WebP against corresponding CPU-encoded WebP, not against PNG. Recorded warm sidecar startup reported 96.8 ms. Rust workspace tests passed (13 total); Python modules compile. Sources remain unchanged.

The 50 ms target is still missed for high-pressure fine/mixed workloads. Eight workers and batching do not remove GPU contention or queueing; the original scattered WebP c32 median regressed in this run. Do not present the mixed sequential median as proof that every resolution/load meets the target. Compact packet extraction is source/view-cache sensitive, and no controlled physical-disk-cold experiment was performed in this round.

## Ablations and research

Worker-count tests covered 1/2/4/8 workers at c8/c32 for fine and coarse requests, with 542 matching pixel hashes per experiment. Eight workers lowered isolated fine sidecar medians, but more workers also raised native execution time. Arbitrary scattered ROI microbatching regressed fine throughput and was rejected. CUDA spin-wait scheduling did not materially improve performance and was rejected. Timing/correctness raw files: sidecar-workers-experiment.json, sidecar-batching-experiment.json, sidecar-spin-experiment.json, packed-view-correctness.json, packed-final-corpus.json.

Primary references: [native NVIDIA tile API and allocation controls](https://docs.nvidia.com/cuda/nvjpeg2000/apireference.html), [CUDA synchronization and scheduling](https://docs.nvidia.com/cuda/cuda-programming-guide/02-basics/asynchronous-execution.html), [JPEG2000 precinct/codeblock options](https://gdal.org/en/stable/drivers/raster/jp2openjpeg.html). Packet regrouping is a locally implemented optimization validated against the corpus; those references do not independently certify it.
'''
for d in datasets:
 report+=f"\n## {d['size']}: original same workload\n\nBefore: {d['before']}. After: {d['after']}. Trace coverage: {d['traceCount']}; trace lookup errors: {len(d['errors'])}; failed expected-status checks: {d['failed']}. Warmup excluded. Pinpoint 18 per format and bursts 32 on mosaic; pinpoint 30 and bursts 64 on sample. Same order, coordinates, formats and drivers as corresponding baseline. Medians average the middle pair; percentiles use nearest rank.\n\n### Backend ms\n\n"+table(headers,d['rows'])
 report+='\n### Client end-to-end ms (complete HTTP body, excludes client image decode)\n\n'+table(headers,d['e2e'])
 report+='\n### GPU backend ms by native reduction\n\n'+table(headers,d['reductions'])
 for name,rows in d['spans'].items():report+='\n### '+name+' (inclusive span ms)\n\n'+table(headers,rows)
 for name,rows in d['stages'].items():report+='\n### GPU attribute: '+name+' (ms)\n\n'+table(headers,rows)
 report+='\nNested spans overlap and cannot be summed. Batch native/parse/raster timing attributes describe shared batch work, not additive per-request work.\n'
(root/'reports/scheduling-before-after.md').write_text(report,encoding='utf-8')
canvas=Path('C:/Users/shaha/.cursor/projects/c-Users-shaha-Documents-GitHub-Intense/canvases/WMTS-concurrent-scheduling.canvas.tsx')
payload=dict(viewport=viewport,datasets=datasets,headers=headers)
canvas.write_text("import {H1,H2,Text,Table,Stack,Select,BarChart,useState} from 'cursor/canvas';\ntype Row=(string|number)[]; type Dataset={size:string;before:string;after:string;rows:Row[];e2e:Row[];reductions:Row[];spans:Record<string,Row[]>;stages:Record<string,Row[]>;traceCount:number;errors:unknown[];failed:number}; const data:{viewport:Row[];datasets:Dataset[];headers:string[]}="+json.dumps(payload)+";\nexport default function Report(){const [asset,setAsset]=useState('0');const [metric,setMetric]=useState('backend');const d=data.datasets[Number(asset)];const [span,setSpan]=useState('river.sidecar.http');const names=Object.keys(d.spans);const selected=names.includes(span)?span:names[0];return <Stack gap={18}><H1>River: 50 ms under concurrent loads</H1><Text>Met: coarse overviews c8/c32; neighboring fine and mixed viewports c8. Still missed: neighboring fine/mixed c32 and scattered bursts. Backend timing includes sidecar waiting. No decoded result cache.</Text><H2>After: backend median latency by workload</H2><Text>Category axis: workload/concurrency. Value axis: backend median milliseconds. Source: viewport-load-after.json, 2026-10-11, 128 measured requests per category.</Text><BarChart categories={data.viewport.map(r=>String(r[0]))} series={[{name:'After backend median',data:data.viewport.map(r=>Number(r[5]))}]} referenceLines={[{value:50,label:'50 ms target'}]} valueSuffix=' ms' height={260}/><H2>Matched viewport ablation: before / after (ms)</H2><Table headers={data.headers} rows={data.viewport}/><Text>Before: packing/batching/reserve disabled, four workers. After: enabled, eight workers. Both retain current lookup normalization and two parsed streams. 128 WebP requests per cell, eight warmups excluded. Adjacent: changing 4 × 2 tile grids; mixed: reductions 6/2/0. Before is an ablation preset rather than the previous binary.</Text><H2>Original workloads: all drivers</H2><Select value={asset} onChange={setAsset} options={data.datasets.map((d,i)=>({value:String(i),label:d.size}))}/><Select value={metric} onChange={setMetric} options={[{value:'backend',label:'Backend server milliseconds'},{value:'e2e',label:'Client complete-body milliseconds'}]}/><Table headers={data.headers} rows={metric==='backend'?d.rows:d.e2e}/><Text>Before: {d.before}. After: {d.after}. Trace coverage {d.traceCount}; zero expected-status failures and trace lookup errors. Same request order/counts/positions/formats as recorded baseline. Small samples make high percentiles equal maxima. Scattered WebP c32 regressed; target is not met universally.</Text><H2>GPU backend by reduction (ms)</H2><Table headers={data.headers} rows={d.reductions}/><H2>Trace span durations (inclusive ms)</H2><Select value={selected} onChange={setSpan} options={names.map(name=>({value:name,label:name}))}/><Table headers={data.headers} rows={d.spans[selected]}/><Text>Nested spans overlap. Batch stage attributes describe shared work, not durations to sum across requests.</Text><H2>Pixel correctness and implementation</H2><Text>35/35 complete reduced catalog images exact, max channel delta 0. 108/108 HTTP format probes exact; 96/96 mixed concurrent PNG probes exact. 100/100 neighboring spatial probes exact at c32, trace-confirmed batches up to four requests. Rust: 13 tests passed. Original compressed LL packets are regrouped into one zero-DWT GPU tile; 451 native submissions become one. Compatible requests coalesce for at most 1 ms, with fine batching limited to identical native footprints. Two parsed source streams per worker, reserved GPU/pinned buffers, exact lookup normalization, fast lossless PNG. No source imagery was modified.</Text><Text>Worker-count and grouping experiments retained pixel hashes; arbitrary scattered batching and CUDA spin wait were rejected. Unsupported layouts retain normal decode. Physical disk caches were not dropped. Full report: reports/scheduling-before-after.md.</Text></Stack>}\n",encoding='utf-8')
print('Wrote scheduling report and canvas')

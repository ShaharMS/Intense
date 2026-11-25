"""Collect exact-image consolidation evidence after successful measured checks."""
import hashlib,json,shutil,subprocess,statistics
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];B=ROOT/'target/benchmarks';T=ROOT/'target/tests';D=ROOT/'evidence/consolidation-m1228';D.mkdir(exist_ok=True)
def load(p):return json.loads(p.read_text())
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def copy(p,folder):
 destination=D/folder/p.name;destination.parent.mkdir(exist_ok=True);shutil.copy2(p,destination);return dict(path=str(destination.relative_to(ROOT)),sha256=sha(destination))
version='m1228-consolidation';build=ROOT/'target/builds'/version;status=load(build/'status.json');assert status['state']=='built'
image=json.loads(subprocess.check_output(['podman','image','inspect','localhost/river:'+version],text=True))[0];identity='sha256:'+image['Id'].removeprefix('sha256:');assert identity==status['imageId']
container=json.loads(subprocess.check_output(['podman','inspect','replica-1'],text=True))[0];assert container['Image'].removeprefix('sha256:')==identity.removeprefix('sha256:') and container['State']['Status']=='running'
main=load(B/'container-comparison.json');assert main['complete'] and len(main['rows'])==360
assert all(r['referencePixelsExact'] is True for r in main['rows'] if r['phase']!='cold_preview')
for name in {r['image'] for r in main['rows']}:
 assert len({r['pixelSha256'] for r in main['rows'] if r['image']==name and r['phase']=='cold_preview'})==1
reports=[B/'container-comparison.json',B/'comparison.html'];ratios={}
for phase in ['cold_zoom','cold_preview','zoom_after_preview','repeated_zoom']:
 values=[]
 for name in {r['image'] for r in main['rows']}:
  med={v:statistics.median(r['ms'] for r in main['rows'] if r['image']==name and r['phase']==phase and r['version']==v) for v in ['m1227','current']};values.append(med['current']/med['m1227'])
 ratios[phase]=dict(medianImageRatio=statistics.median(values),largestInitialImageRatio=max(values))
progressive_count=0
for version_label in ['m1227','current']:
 for rep in ['', '-r2','-r3']:
  for suffix in ['', '-auto-overview']:
   p=B/('progressive-'+version_label+rep+suffix+'.json');d=load(p);assert d['complete'] and len(d['rows'])==15 and all(r['referencePixelsExact'] for r in d['rows']);reports.append(p);progressive_count+=len(d['rows'])
for name in ['urban-followup.json','terrain-followup.json','layers-followup.json']:
 p=B/name;d=load(p);assert d['complete'] and len(d['rows'])==64;reports.append(p)
for name in ['edge-m1227.json','edge-current.json']:
 p=B/name;d=load(p);assert len(d['rows'])==20;assert sum(bool(r.get('exact')) for r in d['rows'])==19;assert all(r.get('exact') or r.get('status')==422 and r['requestedDriver']=='openjpeg' and r['file'].endswith('.jph') for r in d['rows']);reports.append(p)
checks=['cors-final.json','tls-final.json','tls-pfx-final.json','container-read-feedback.json','container-retry-feedback.json','container-shared-cache.json','container-pressure.json','container-local.json','cache-learning.json','local-lookup.json']
for name in checks:assert load(T/name)['passed'];reports.append(T/name)
for name in ['http3-final.log','http3-pfx-final.log']:assert 'HTTP/3 negotiated' in (T/name).read_text();reports.append(T/name)
unit=ROOT/'target/tests-feedback-final.log';assert '326 passed; 0 failed; 16 ignored' in unit.read_text();reports.append(unit)
for name in ['corpus.json','edge-corpus.json','structure-verification.json']:reports.append(ROOT/'target/fixtures'/name)
reports.append(build/'manifest.json')
records=[copy(p,'measurements' if p.parent==B else 'checks') for p in reports]
failure=B/'extended-baseline-timeout/progressive-m1227-r2.log';records.append(copy(failure,'extended-baseline-timeout'))
proof=dict(qualifiedForRelease=True,version='m1228-consolidation',imageId=identity,sourceManifestSha256=sha(build/'manifest.json'),channels=['stable','lts','latest'],channelPolicy='One qualified image; explicitly requested consolidation and shared maintenance, no claimed 10% speedup',baseline=dict(version='m1227',originalPublishedImage=True,archiveSha256=sha(ROOT/'target/baseline/river-m1227-runtime.zip')),httpMeasurements=360,progressiveStreams=progressive_count,additionalPairedMeasurements=192,unitTests=dict(passed=326,ignored=16,realRedisValkeyIntegration='separate live server and container restart checks passed'),performance=ratios,correctness='Independent full-resolution and progressive final pixels exact; HTTP preview pixels exact across versions; 19/20 edge driver paths exact, explicit OpenJPEG HT rejection expected',resourceSafety='Matched four-CPU/two-GiB containers; pressure contraction and recovery without OOM; RAM-only scratch; bounded transfer rates and explicit ceilings; native 512-MiB concurrency/lookup test',cacheLearning=dict(signals=['measured producer work reused per resident byte','measurement coverage','reuse','eviction pressure','occupancy'],measuredCategories=7,unavailableCategories=['indexes','asset_metadata'],liveRequests=load(T/'cache-learning.json')['requests'],scope='Sustained native workload on identical cache-learning implementation plus image diagnostics; no inferred end-to-end gain'),overfetch=dict(originAccountingExact=True,configuredWarmupRespected=True,successfulRetriesExcluded=True,scope='Forty cold object aliases on one controlled source; needed bytes are unique consumer-requested intersections, not theoretical entropy',observedRatio=load(T/'container-read-feedback.json')['rows'][-1]['source']['actualOverfetch']['measuredRatio'],learnedOptionalRatio=load(T/'container-read-feedback.json')['rows'][-1]['source']['actualOverfetch']['effectiveOptionalReadRatio'],bandwidthGainClaimed=False),knownLimitations=['No NVIDIA GPU was available; CPU drivers and disclosed fallbacks were exercised.','Synthetic fixtures cannot qualify every customer codestream.','Index and image-metadata time benefits remain unavailable; nested saved-work estimates are not additive.','An extended baseline m1227 run timed out on memory admission; both versions restart between matched progressive batches. Failure evidence is retained.','Intermittent source-read latency outliers remain in raw results; eight-pair followups are reported separately.','Overfetch feedback is applied in adaptive mode; observe remains observational. Mandatory reads can exceed the optional ratio target.','Progressive timings measure frame arrival after manifest preparation, not browser paint.'],evidence=records)
(D/'qualification.json').write_text(json.dumps(proof,indent=2)+'\n');print(json.dumps(dict(qualified=True,imageId=identity,evidence=str(D/'qualification.json'))))

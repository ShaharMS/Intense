"""Render measured release comparisons without replacing outliers or inventing gains."""
import html,json,statistics
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];B=ROOT/'target/benchmarks'
load=lambda name:json.loads((B/name).read_text())
main=load('container-comparison.json');assert main['complete'] and len(main['rows'])==360
progress={v:load('progressive-'+v+'.json') for v in ['m1227','current']}
overviews={v:load('progressive-'+v+'-auto-overview.json') for v in progress}
# Aggregate three runs without dropping the original observations.
for suffix,group in [('',progress),('-auto-overview',overviews)]:
 for version in group:
  reps=[load('progressive-'+version+rep+suffix+'.json') for rep in ['', '-r2','-r3']]
  assert all(r['complete'] for r in reps)
  for i,row in enumerate(group[version]['rows']):
   for key in ['manifestMs','firstFrameMs','fullPreviewMs','finalMs']:
    values=[r['rows'][i][key] for r in reps if r['rows'][i][key] is not None]
    row[key]=statistics.median(values) if values else None
fixtures=json.loads((ROOT/'target/fixtures/corpus.json').read_text())['fixtures']
out=['<!doctype html><meta charset="utf-8"><title>River m1227 versus m1228 consolidation</title><style>body{font:16px system-ui;max-width:1250px;margin:32px auto;padding:0 20px;color:#17212b}table{border-collapse:collapse;width:100%;margin:16px 0 40px}td,th{border:1px solid #ccd4dc;padding:8px;text-align:left}th{background:#e8f0f5}small{color:#435465}code{background:#f0f3f5}h2{margin-top:48px}</style><h1>River m1227 → m1228 consolidation</h1>']
out+=['<p>Original published m1227 image versus the consolidated image, same 4-CPU quota and 2 GiB memory limit. Offline-generated 4096² RGB images, five lossless optimization layouts each. Origin: 7 ms response delay, shared 300 MB/s body budget. Source filesystem is warm. Cold means a fresh source/object identity in a running process, not a cold OS or browser.</p>', '<p>HTTP cells are medians of three alternating matched pairs. Progressive rows are medians of three runs per version; frame times start after separately reported manifest preparation. Each 15-image progressive batch restarts its backend; initial decoder setup is included where encountered. They measure frame arrival, not browser paint. No universal speedup is claimed. GETs and bytes come from the controlled origin, not estimated decoder ranges. All full-resolution and final pixels match independent references; HTTP previews also match each other byte-for-byte in decoded pixels.</p>']
def table(headers,rows):return '<table><thead><tr>'+''.join('<th>'+html.escape(h)+'</th>' for h in headers)+'</tr></thead><tbody>'+''.join('<tr>'+''.join('<td>'+str(c)+'</td>' for c in row)+'</tr>' for row in rows)+'</tbody></table>'
def metric(rows):return f"{statistics.median(r['ms'] for r in rows):.2f} ms"
def wire(rows):
 vals={(r['wire']['gets'],r['wire']['bytes']) for r in rows}
 return '<br>'.join(f'{g} GET / {b:,} B' for g,b in sorted(vals))
summary=[]
for phase in ['cold_zoom','cold_preview','zoom_after_preview','repeated_zoom']:
 ratios=[]
 for f in fixtures:
  med={v:statistics.median(r['ms'] for r in main['rows'] if r['image']==f['file'] and r['phase']==phase and r['version']==v) for v in progress};ratios.append(med['current']/med['m1227'])
 summary.append([phase,f'{statistics.median(ratios):.3f}×',f'{max(ratios):.3f}×'])
out+=[table(['HTTP phase','Median per-image current/baseline ratio','Largest initial per-image ratio'],summary),'<p>Initial outliers were investigated, not removed. Additional alternating pairs check intermittent source-read stalls. Their medians are included below; the full 360-row comparison remains intact. An extended m1227 progressive run hit memory-admission timeout after accumulated benchmark churn; the failure log is retained. Both versions restart between matched progressive batches for comparable process history.</p>']
for f in fixtures:
 name=f['file'];out.append('<h2>'+html.escape(name)+'</h2>');out.append('<p>Optimization metadata: <code>'+html.escape(json.dumps({k:v for k,v in f.items() if k not in ['sha256','sourceSha256']},sort_keys=True))+'</code></p>');rows=[]
 for phase in ['cold_zoom','cold_preview','zoom_after_preview','repeated_zoom']:
  groups={v:[r for r in main['rows'] if r['image']==name and r['phase']==phase and r['version']==v] for v in progress}
  rows.append([phase,metric(groups['m1227']),metric(groups['current']),wire(groups['m1227']),wire(groups['current']),'Exact reference' if phase!='cold_preview' else 'Exact cross-version pixels'])
 for title,data in [('512² progressive',progress),('4096² overview / Auto',overviews)]:
  rs={v:next(r for r in data[v]['rows'] if r['image']==name) for v in progress}
  for label,key in [('manifest','manifestMs'),('first frame','firstFrameMs'),('full preview','fullPreviewMs'),('final','finalMs')]:
   vals=[f'{rs[v][key]:.2f} ms' if rs[v][key] is not None else 'No preview pass' for v in progress]
   traffic=[]
   for v in progress:
    w={k:rs[v]['wireIncludingManifest'][k]-rs[v]['wireAfterManifest'][k] for k in rs[v]['wireIncludingManifest']} if key=='manifestMs' else rs[v]['wireAfterManifest'];traffic.append(f"{w['gets']} GET / {w['bytes']:,} B"+(' (manifest)' if key=='manifestMs' else ' (entire stream)'))
   rows.append([title+' — '+label,*vals,*traffic,'Final exact reference; stage timing only'])
  out.append('<p>'+title+' actual final decoder: '+html.escape(' / '.join(v+': '+str(rs[v]['frames'][-1]['header'].get('decoderBackend',rs[v]['frames'][-1]['header'].get('serverTiming',{}).get('decoderBackend','see raw frame headers'))) for v in progress))+'</p>')
 out.append(table(['Request / stage','m1227','m1228','m1227 origin traffic','m1228 origin traffic','Pixel check'],rows))
for name in ['urban-followup.json','terrain-followup.json','layers-followup.json']:
 d=load(name);assert d['complete'];rows=[]
 for phase in ['cold_zoom','cold_preview','zoom_after_preview','repeated_zoom']:
  groups={v:[r for r in d['rows'] if r['phase']==phase and r['version']==v] for v in progress};rows.append([phase,metric(groups['m1227']),metric(groups['current']),wire(groups['m1227']),wire(groups['current'])])
 out+=['<h2>Eight-pair follow-up: '+html.escape(d['rows'][0]['image'])+'</h2>',table(['Request','m1227','m1228','m1227 origin','m1228 origin'],rows)]
out.append('<h2>Scope and limitations</h2><p>Edge fixtures cover 16-bit grayscale, declared alpha, six/eight multispectral bands and HTJ2K, with explicit driver and Auto checks. OpenJPEG correctly rejects HT blocks; other routes preserve exact output and disclose fallbacks. No NVIDIA GPU is available here. Generated imagery cannot prove performance for every customer codestream. Cache-benefit timings estimate previously measured work reused; nested caches overlap. The 40-object overfetch fixture verified exact byte accounting and bounded adjustment but did not reduce its required reads. Raw evidence retains timing headers, source ranges, hashes and decisions.</p>')
(B/'comparison.html').write_text('\n'.join(out));print(B/'comparison.html')

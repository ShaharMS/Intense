"""Explain every real source range for the frozen three-size suite; diagnostic, not a release benchmark."""
import argparse,importlib.util,io,json,socket,struct,subprocess,threading,time,uuid
from pathlib import Path
from urllib.parse import urlencode
from http.server import ThreadingHTTPServer
from PIL import Image
spec=importlib.util.spec_from_file_location('fixed',Path(__file__).with_name('benchmark-fixed.py'));fixed=importlib.util.module_from_spec(spec);spec.loader.exec_module(fixed)

def metadata_ranges(path):
    size=path.stat().st_size
    with path.open('rb') as f:
        def read(at,n):f.seek(at);v=f.read(n);assert len(v)==n;return v
        at=0
        while True:
            length,kind=struct.unpack('>I4s',read(at,8));header=8
            if length==1:length=struct.unpack('>Q',read(at+8,8))[0];header=16
            if length==0:length=size-at
            assert length>=header and at+length<=size
            if kind==b'jp2c':break
            at+=length
        start=at+header;end=at+length;assert read(start,2)==b'\xffO';cursor=start+2
        while read(cursor,2)!=b'\xff\x90':
            n=struct.unpack('>H',read(cursor+2,2))[0];assert n>=2;cursor+=n+2
        ranges=[(0,cursor)]
        while cursor<end:
            assert read(cursor,2)==b'\xff\x90';part=read(cursor,12);assert struct.unpack('>H',part[2:4])[0]==10
            length=struct.unpack('>I',part[6:10])[0];stop=cursor+length if length else end
            marker=cursor+12
            while read(marker,2)!=b'\xff\x93':
                n=struct.unpack('>H',read(marker+2,2))[0];assert n>=2;marker+=n+2;assert marker<stop
            ranges.append((cursor,marker+2));cursor=stop
            if cursor+2==end and read(cursor,2)==b'\xff\xd9':break
        return ranges

def main():
    p=argparse.ArgumentParser();p.add_argument('--manifest',type=Path,default=Path('reports/fixed-gate-v2/manifest.json'));p.add_argument('--output',type=Path,required=True);p.add_argument('--expected-image',default='river:stable');p.add_argument('--tcp-nodelay',action='store_true');p.add_argument('--origin-endpoint');p.add_argument('--stats-url');p.add_argument('--driver',choices=['openjpeg','jp2grok','openhtj2k']);a=p.parse_args();assert bool(a.origin_endpoint)==bool(a.stats_url);assert not a.output.exists()
    manifest=json.loads(a.manifest.read_text());cases=manifest['cases'];files={c['name']:(a.manifest.parent/c['file']).resolve() for c in cases};metadata={k:metadata_ranges(v) for k,v in files.items()}
    counters={};lock=threading.Lock();traces={};base=fixed.make_origin(files,cases,manifest,counters,lock)
    class Origin(base):
        def setup(self):
            super().setup()
            if a.tcp_nodelay:self.connection.setsockopt(socket.IPPROTO_TCP,socket.TCP_NODELAY,1)
        def serve(self,body):
            key=self.path.strip('/').split('/')[0];start=time.perf_counter();entry={'range':self.headers.get('Range'),'body':body,'start':start}
            with lock:traces.setdefault(key,[]).append(entry)
            try:super().serve(body)
            finally:entry['end']=time.perf_counter();entry['durationMs']=(entry['end']-start)*1000
    server=None
    if a.origin_endpoint:
        assert not a.tcp_nodelay,'External origins set their own TCP options'
        description=json.loads(fixed.http(a.stats_url+'/__manifest'));assert description['sha256']==fixed.sha(a.manifest.read_bytes())
    else:
        server=ThreadingHTTPServer(('0.0.0.0',18995),Origin);threading.Thread(target=server.serve_forever,daemon=True).start()
    def stats(sid):
        if a.stats_url:return json.loads(fixed.http(a.stats_url+'/__stats/'+sid))
        return {'wire':dict(counters[sid]),'ranges':traces.get(sid,[])}
    api='http://127.0.0.1:18882';container='river-goal-fixed-candidate';identity=json.loads(subprocess.check_output(['docker','inspect',container]))[0]['Image'];stable=subprocess.check_output(['docker','image','inspect',a.expected_image,'--format','{{.Id}}'],text=True).strip();assert identity==stable
    report={'driverOverride':a.driver,'originEndpoint':a.origin_endpoint or 'Windows host HTTP origin','originTcpNoDelay':a.tcp_nodelay,'diagnosticOnly':True,'promotionEligible':False,'imageId':identity,'manifestSha256':fixed.sha(a.manifest.read_bytes()),'cases':[]}
    try:
        for c in cases:
            subprocess.run(['docker','restart',container],check=True,stdout=subprocess.DEVNULL)
            for _ in range(100):
                try:fixed.http(api+'/api/drivers');break
                except OSError:time.sleep(.1)
            sid='trace-'+uuid.uuid4().hex;endpoint=(a.origin_endpoint or 'http://host.docker.internal:18995').rstrip('/')+'/'+sid;counters[sid]={'gets':0,'heads':0,'bytes':0}
            try:
                fixed.http(api+'/api/s3/sources',dict(id=sid,name=sid,bucket='fixture',region='us-east-1',endpoint=endpoint,accessKeyId='fixture',secretAccessKey='fixture',allowHttp=True),'POST');assert stats(sid)['wire']==dict(gets=0,heads=0,bytes=0)
                query=dict(image=endpoint+'/fixture/'+c['name']+'.jp2',driver=a.driver or c['driver'],edge='crop',tileSize=c['tileSize'],minZoom=0,maxZoom=c['maxZoom'],tilePixelRatio=1)
                if 'layers' in c:query['layers']=c['layers']
                url=api+f"/streamer/v1/{c['z']}/{c['x']}/{c['y']}/tile.png?"+urlencode(query)
                started=time.perf_counter();body,headers=fixed.http(url,include_headers=True);elapsed=(time.perf_counter()-started)*1000
                actual=Image.open(io.BytesIO(body)).convert('RGB');ref=Image.open(a.manifest.parent/c['reference']).convert('RGB');assert actual.size==ref.size and actual.tobytes()==ref.tobytes()
                before=stats(sid)['wire'];assert fixed.http(url)==body;assert stats(sid)['wire']==before
                summary={'metadataOnlyGets':0,'mixedGets':0,'entropyOnlyGets':0,'metadataBytes':0,'entropyBytes':0};entries=stats(sid)['ranges']
                for e in entries:
                    if a.stats_url:
                        e['originStartSeconds']=e.pop('start');e['originEndSeconds']=e.pop('end')
                    else:
                        e['startMs']=(e.pop('start')-started)*1000;e['endMs']=(e.pop('end')-started)*1000
                    if not e['body']:continue
                    first,last=e['range'].removeprefix('bytes=').split('-');lo=int(first);hi=min(int(last)+1,files[c['name']].stat().st_size)
                    meta=sum(max(0,min(hi,y)-max(lo,x)) for x,y in metadata[c['name']]);entropy=hi-lo-meta
                    e.update(metadataBytes=meta,entropyBytes=entropy)
                    summary['metadataBytes']+=meta;summary['entropyBytes']+=entropy
                    summary['metadataOnlyGets' if entropy==0 else 'entropyOnlyGets' if meta==0 else 'mixedGets']+=1
                row=dict(case=c['name'],coldMs=elapsed,wire=before,summary=summary,pixelsExact=True,repeatSourceIoZero=True,responseTimingHeaders={k:v for k,v in headers.items() if k.lower().startswith('x-river-')},ranges=entries);report['cases'].append(row);print(c['name'],before,summary,flush=True)
            finally:fixed.http(api+'/api/s3/sources/'+sid,method='DELETE')
        report['passed']=True
    finally:
        a.output.write_text(json.dumps(report,indent=2)+'\n')
        if server:server.shutdown()
if __name__=='__main__':main()

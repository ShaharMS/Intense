import {writeFile} from 'node:fs/promises';
import {performance} from 'node:perf_hooks';
const api=process.env.RIVER_API??'http://127.0.0.1:18080',label=process.env.RIVER_RUN_LABEL??'after';
const asset='034a2b0c177ea5f58cc275822c21c4f2919ee2b770661de8076dba326a464299';
const samples=[];
const stats=v=>{v=v.sort((a,b)=>a-b);return {n:v.length,mean:v.reduce((a,b)=>a+b,0)/v.length,median:(v[Math.floor((v.length-1)/2)]+v[Math.floor(v.length/2)])/2,p95:v[Math.ceil(v.length*.95)-1],p99:v[Math.ceil(v.length*.99)-1]}};
function url(reduction,col,row){const q=new URLSearchParams({SERVICE:'WMTS',REQUEST:'GetTile',VERSION:'1.0.0',LAYER:asset,STYLE:'default',FORMAT:'image/webp',DRIVER:'nvjpeg2000',TILEMATRIXSET:`RiverPixel-${asset}`,TILEMATRIX:String(6-reduction),TILEROW:String(row),TILECOL:String(col)});return `${api}/wmts?${q}`;}
async function run(i,kind,concurrency,warmup=false){
 const reduction=kind==='overview'?6:kind==='mixed'?[6,2,0][i%3]:0;
 const edge=512*2**reduction,cols=Math.ceil(40980/edge),rows=Math.ceil(10980/edge);
 // Four-by-two neighboring tiles, changing viewport origin every eight requests.
 const origin=Math.floor(i/8)*4,col=(origin+i%4)%cols,row=Math.floor(i%8/4)%rows;
 const start=performance.now();const response=await fetch(url(reduction,col,row),{signal:AbortSignal.timeout(120000)});const bytes=await response.arrayBuffer();
 samples.push({kind,concurrency,index:i,reduction,col,row,warmup,status:response.status,bytes:bytes.byteLength,e2eMs:performance.now()-start,serverMs:Number(response.headers.get('x-river-server-ms')),decodeMs:Number(response.headers.get('x-river-decode-ms')),encodeMs:Number(response.headers.get('x-river-encode-ms')),traceId:response.headers.get('x-river-trace-id')});
 if(!response.ok)throw Error(`HTTP ${response.status}`);
}
for(const kind of ['overview','adjacent','mixed']){
 for(let i=0;i<8;i++)await run(i,kind,1,true);
 for(const concurrency of [8,32]){let next=0;await Promise.all(Array.from({length:concurrency},async()=>{while(next<128)await run(next++,kind,concurrency);}));}
}
const summary={};for(const s of samples.filter(s=>!s.warmup)){const key=`${s.kind}/c${s.concurrency}`;(summary[key]??=[]).push(s);}
for(const [key,value] of Object.entries(summary))summary[key]=Object.fromEntries(['e2eMs','serverMs','decodeMs','encodeMs'].map(k=>[k,stats(value.map(s=>s[k]))]));
const path=`reports/viewport-load-${label}.json`;await writeFile(path,JSON.stringify({at:new Date().toISOString(),label,api,samples,summary},null,2));console.log(JSON.stringify({path,summary},null,2));

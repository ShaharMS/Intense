import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const source=fs.readFileSync(new URL('../web/viewer.js',import.meta.url),'utf8');
const calls=[];let socket;let releaseCatalog;
const context=vm.createContext({URL,apiBase:'http://fixture',session:null,console,
 WebSocket:class {constructor(){socket=this;}},
 loadS3Sources:async()=>{calls.push('sources');},
 refreshAvailableAssets:()=>{calls.push('catalog-start');return new Promise(resolve=>{releaseCatalog=resolve;});},
 refreshViewerOptions:async()=>{calls.push('drivers');},
 loadAsset:async()=>{calls.push('image');},
});
vm.runInContext(source.slice(source.indexOf('function connect()'),source.indexOf('function handleControl(')),context);
context.connect();let timer;
await Promise.race([socket.onopen(),new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('Image loading waited for stalled catalog')),100);})]);
clearTimeout(timer);
assert.deepEqual(calls,['sources','catalog-start','drivers','image']);
releaseCatalog();
console.log('PASS: image and driver loading proceed while connected S3 catalog search remains pending');

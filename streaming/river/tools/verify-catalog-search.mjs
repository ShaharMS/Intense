import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const source=fs.readFileSync(new URL('../web/viewer.js',import.meta.url),'utf8');
const refresh=source.slice(source.indexOf('async function refreshAvailableAssets('),source.indexOf("catalogNext.addEventListener('click'"));
const search=source.slice(source.indexOf('function searchCatalog()'),source.indexOf("catalogSearch.addEventListener('input', searchCatalog)"));
const requests=[];const timers=new Map();let clock=0;
const context=vm.createContext({URLSearchParams,AbortController,
 apiBase:'http://fixture',catalogGeneration:0,catalogAbort:null,catalogPageHistory:[],catalogPageStart:null,catalogLoading:false,catalogError:'',
 catalogSource:{value:'all'},catalogSearch:{value:'first'},catalogSearchMode:{value:'contains'},
 catalogNext:{disabled:false},catalogPrevious:{disabled:true},catalogCount:{textContent:''},catalogList:{replaceChildren(){}},
 catalogCursor:'stale',availableAssets:[],catalogSearchTimer:null,renderCatalog(){},
 setTimeout(fn,delay){if(delay===100){setImmediate(fn);return 0;}timers.set(++clock,fn);return clock;},
 clearTimeout(id){timers.delete(id);},fetch(url,options){return new Promise(resolve=>requests.push({url,options,resolve}));}
});
vm.runInContext(refresh+'\n'+search,context);
const flush=()=>new Promise(resolve=>setTimeout(resolve,5));
const reply=(index,objects,nextCursor)=>requests[index].resolve({ok:true,json:async()=>({objects,nextCursor,scannedObjects:50})});
const objects=Array.from({length:50},(_,i)=>({key:`image${i}.jp2`}));
context.searchCatalog();context.catalogSearch.value='needle';context.searchCatalog();
assert.equal(timers.size,1);assert.equal(requests.length,0);[...timers.values()][0]();timers.clear();
assert.equal(new URL(requests[0].url).searchParams.get('source'),'all');
assert.equal(new URL(requests[0].url).searchParams.get('q'),'needle');
reply(0,[],'scan-more');await flush();
assert.equal(new URL(requests[1].url).searchParams.get('cursor'),'scan-more');
reply(1,objects,'page2');await flush();
assert.equal(context.availableAssets.length,50);assert.equal(context.catalogNext.disabled,false);
const next=context.refreshAvailableAssets({next:true});reply(2,objects,'page3');await next;
assert.equal(context.catalogPrevious.disabled,false);
const previous=context.refreshAvailableAssets({previous:true});
assert.equal(new URL(requests[3].url).searchParams.has('cursor'),false);reply(3,objects,'page2');await previous;
assert.equal(context.catalogPrevious.disabled,true);
const stale=context.refreshAvailableAssets({next:true});context.catalogSearch.value='changed';context.searchCatalog();
assert.equal(requests[4].options.signal.aborted,true);[...timers.values()][0]();timers.clear();
reply(5,[{key:'changed.jp2'}],null);await flush();reply(4,objects,'old');await stale;
assert.equal(context.availableAssets.length,1);assert.equal(context.catalogCursor,null);
console.log('PASS: all-S3 search, fill 50 matches across scan continuations, next/previous, debounce and stale cancellation');

import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const source=fs.readFileSync(new URL('../web/viewer.js',import.meta.url),'utf8');
const start=source.indexOf('function createRenderer(target)');
const end=source.indexOf('\nfunction linkProgram',start);
const draws=[];const frames=[];let texture;let vertices;let serial=0;
const gl=new Proxy({createTexture:()=>({id:++serial}),createBuffer:()=>({}),getAttribLocation:()=>0,
  bindTexture:(_,value)=>{texture=value;},bufferData:(_,value)=>{vertices=Array.from(value);},
  drawArrays:()=>draws.push({id:texture.id,vertices})},{get:(obj,key)=>obj[key]??(()=>{})});
const context=vm.createContext({performance,Float32Array,Map,RIVER_TILE_SIZE:512,linkProgram:()=>({}),
  requestAnimationFrame:callback=>{frames.push(callback);return frames.length;},cancelAnimationFrame:()=>{}});
vm.runInContext(source.slice(start,end),context);
const renderer=context.createRenderer({getContext:()=>gl});
const viewport={assetId:'large',x:0,y:0,zoom:5};
const layout={scale:32,offsetX:0,offsetY:0,stageWidth:1280,stageHeight:344};
renderer.begin(viewport,layout);
// A late preview must remain below an already received final tile.
const final=renderer.draw({tileX:0,tileY:0,pass:'final',outputWidth:512,outputHeight:344,sourceLayers:1},{},viewport,layout);
const patch=renderer.draw({tileX:3,tileY:1,sourceTileEdge:2048,preview:true,pass:'preview',outputWidth:64,outputHeight:64,sourceLayers:1},{},viewport,layout);
frames.splice(0).forEach(callback=>callback(1));
await Promise.all([final.presented,patch.presented]);
assert.deepEqual(draws.map(draw=>draw.id),[2,1]);
// Patch source origin is (6144,2048), hence display origin (192,64), not (1536,512).
assert.ok(Math.abs(draws[0].vertices[0]-(-0.701171875))<1e-6);
assert.ok(Math.abs(draws[0].vertices[4]-(-0.598828125))<1e-6);
assert.ok(Math.abs(draws[0].vertices[1]-(1-128.75/344*2))<1e-6);
assert.ok(Math.abs(draws[0].vertices[9]-(1-63.25/344*2))<1e-6);
draws.length=0;
const nextViewport={...viewport,assetId:'different-asset'};
renderer.begin(nextViewport,layout);
renderer.reposition(nextViewport,layout);
frames.splice(0).forEach(callback=>callback(1));
assert.equal(draws.length,0,'same-zoom asset switch retained previous image textures');
renderer.clear();
console.log('PASS: preview placement, final ordering, same-zoom asset switching and explicit clearing');

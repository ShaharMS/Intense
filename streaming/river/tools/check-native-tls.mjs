import assert from 'node:assert/strict';
import fs from 'node:fs';
import http2 from 'node:http2';
import https from 'node:https';
import crypto from 'node:crypto';
import { once } from 'node:events';

const [port, caFile, mode] = process.argv.slice(2);
const http1 = mode !== 'h2-h3';
const ca = fs.readFileSync(caFile);
const authority = 'https://127.0.0.1:' + port;
const session = http2.connect(authority, {ca});
session.setTimeout(8000, () => session.destroy(new Error('HTTP/2 timeout')));
try {
  await once(session, 'connect');
  assert.equal(session.alpnProtocol, 'h2');
  const origin = 'https://viewer.example';
  const req = session.request({':path':'/health',origin});
  req.end();
  const [headers] = await once(req,'response');
  assert.equal(headers[':status'],200);
  assert.equal(headers['access-control-allow-origin'],origin);
  if (mode) assert.equal(headers['alt-svc'], 'h3=":'+port+'"; ma=86400');
  req.resume();
  await once(req,'end');
  const ws = session.request({':method':'CONNECT',':protocol':'websocket',':path':'/ws',
                              'sec-websocket-version':'13'},{endStream:false});
  const [wsHeaders] = await once(ws,'response');
  assert.equal(wsHeaders[':status'],200);
  ws.close(http2.constants.NGHTTP2_CANCEL);
} finally {
  session.destroy();
}

await new Promise((resolve,reject) => {
  const req = https.get(authority+'/health',{ca},response => {
    try {
      assert.equal(response.httpVersion,'1.1');
      assert.equal(response.statusCode,http1 ? 200 : 505);
      response.resume();
      response.on('end',resolve);
    } catch (error) { reject(error); }
  });
  req.setTimeout(8000,() => req.destroy(new Error('HTTPS timeout')));
  req.on('error',reject);
});

if (http1) await new Promise((resolve,reject) => {
  const headers = {
    Connection:'Upgrade', Upgrade:'websocket',
    'Sec-WebSocket-Key':crypto.randomBytes(16).toString('base64'),
    'Sec-WebSocket-Version':'13'
  };
  const req = https.request(authority+'/ws',{ca,headers});
  req.setTimeout(8000,() => req.destroy(new Error('WSS timeout')));
  req.on('upgrade',(res,socket) => {
    socket.destroy();
    try { assert.equal(res.statusCode,101); resolve(); } catch (error) { reject(error); }
  });
  req.on('response',res=>{res.resume();reject(new Error('WSS upgrade returned '+res.statusCode));});
  req.on('error',reject);
  req.end();
});
console.log(JSON.stringify({h2:true,h2WebSocket:true,h1:http1,wss:http1,cors:true,verifiedCertificate:true}));

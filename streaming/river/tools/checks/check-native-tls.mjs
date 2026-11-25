import assert from 'node:assert/strict';
import fs from 'node:fs';
import http2 from 'node:http2';
import https from 'node:https';
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

console.log(JSON.stringify({h2:true,h1:http1,cors:true,verifiedCertificate:true}));

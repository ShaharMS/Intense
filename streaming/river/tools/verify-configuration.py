"""Native API regression: migration, atomic replace, no auth and persistence."""
import argparse,concurrent.futures,copy,json,os,pathlib,socket,subprocess,tempfile,time
from urllib.request import Request,urlopen
from urllib.error import HTTPError
p=argparse.ArgumentParser();p.add_argument('--exe',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
exe=a.exe.resolve();root=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='river-configuration-') as directory:
 folder=pathlib.Path(directory);legacy=folder/'s3.json';saved=folder/'configuration.json'
 source=dict(id='legacy',name='Legacy',bucket='fixture',region='us-east-1',endpoint='http://127.0.0.1:9',accessKeyId='fixture-key',secretAccessKey='fixture-secret',sessionToken='fixture-session',allowHttp=True)
 legacy.write_text(json.dumps([source]));(folder/'encoding.json').write_text('{"webpMethod":3}')
 with socket.socket() as probe:probe.bind(('127.0.0.1',0));port=probe.getsockname()[1]
 api=f'http://127.0.0.1:{port}';env=os.environ.copy();env.update(RIVER_BIND=f'127.0.0.1:{port}',RIVER_S3_CONNECTIONS_FILE=str(legacy),RIVER_CONFIGURATION_FILE=str(saved),RIVER_ENABLE_NVIDIA='false',RIVER_OTEL_ENABLED='false',RIVER_ADMIN_TOKEN='ignored-legacy-token',RIVER_ASSET_CATALOG='',RIVER_S3_ADAPTIVE_READS='observe')
 process=None;log=(folder/'server.log').open('w')
 def call(path,body=None,method=None,expected=200):
  req=Request(api+path,data=None if body is None else json.dumps(body).encode(),method=method,headers={'Content-Type':'application/json'})
  try:r=urlopen(req,timeout=15)
  except HTTPError as e:r=e
  with r:
   data=r.read();assert r.status==expected,(r.status,data.decode(errors='replace'));return (json.loads(data) if 'json' in r.headers.get('Content-Type','') else data.decode()) if data else None
 def start():
  global process
  process=subprocess.Popen([str(exe)],cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT)
  for _ in range(200):
   if process.poll() is not None:raise AssertionError((folder/'server.log').read_text())
   try:call('/health');return
   except OSError:time.sleep(.05)
  raise AssertionError('Server startup timed out')
 def stop():
  process.terminate();process.wait(timeout=15)
 try:
  start();export=call('/api/settings/configuration');assert export['encoding']['webpMethod']==3 and export['sources'][0]['secretAccessKey']=='fixture-secret' and export['sources'][0]['sessionToken']=='fixture-session'
  replacement=copy.deepcopy(export);replacement['encoding']['webpMethod']=1;replacement['readPolicy']['mode']='adaptive'
  assert call('/api/settings/configuration',replacement,'PUT')['persisted'];assert call('/api/settings/configuration')==replacement
  assert json.loads(saved.read_text())==replacement
  for change in ('version','duplicate','policy','source','unknown'):
   bad=copy.deepcopy(replacement)
   if change=='version':bad['schemaVersion']=2
   if change=='duplicate':bad['sources']*=2
   if change=='policy':bad['readPolicy']['maxOverfetchRatio']=99
   if change=='source':bad['sources'][0]['id']='!invalid!'
   if change=='unknown':bad['unexpected']=True
   call('/api/settings/configuration',bad,'PUT',expected=422 if change=='unknown' else 400)
   assert call('/api/settings/configuration')==replacement and json.loads(saved.read_text())==replacement
  # A failed atomic rename must not publish any part of the imported document.
  backup=folder/'previous.json';saved.rename(backup);saved.mkdir()
  try:
   bad=copy.deepcopy(replacement);bad['encoding']['webpMethod']=6
   call('/api/settings/configuration',bad,'PUT',expected=500);assert call('/api/settings/configuration')==replacement
  finally:saved.rmdir();backup.rename(saved)
  def add(i):
   value=source|dict(id=f'source-{i}',name=f'Source {i}');call('/api/s3/sources',value,'POST')
  with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:list(pool.map(add,range(4)))
  current=call('/api/settings/configuration');assert len(current['sources'])==5 and current['readPolicy']['mode']=='adaptive'
  call('/api/settings/encoding',{'webpMethod':2},'PUT');policy={'mode':'observe','minimumSamples':32,'maxOverfetchRatio':1};call('/api/settings/read-policy',policy,'PUT')
  current=call('/api/settings/configuration');assert current['encoding']['webpMethod']==2 and current['readPolicy']==policy
  stop();start();assert call('/api/settings/configuration')==current;assert call('/api/settings/read-policy')['settings']==policy
  assert call('/api/settings/configuration',current,'PUT')['applied']
  report=dict(passed=True,noAuthorizationRequired=True,credentialsExported=True,legacyMigrated=True,rejectedImportsPreserveState=True,persistenceFailurePreservesState=True,concurrentUpdatesPreserved=True,restartRestoresWholeDocument=True,roundTrip=True)
  a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(report,indent=2));print(json.dumps(report))
 finally:
  if process is not None and process.poll() is None:stop()
  log.close()

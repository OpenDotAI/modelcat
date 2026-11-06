#!/usr/bin/env python3
"""Native buyer installers, tested only with a local model-list fixture and isolated files."""
import http.server,json,os,subprocess,tempfile,threading
from pathlib import Path

root=Path(__file__).resolve().parents[2]
binary=Path(os.environ.get('MODELCAT_TEST_BINARY',root/'bin/modelcat'))
class API(http.server.BaseHTTPRequestHandler):
 models=['fixture-codex','claude-fixture','fixture-dsh']
 calls=[]
 def do_GET(self):
  self.calls.append(self.path)
  if self.path!='/v1/models' or self.headers.get('Authorization')!='Bearer fixture-only-key':self.send_error(401);return
  self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(json.dumps({'data':[{'id':i} for i in self.models]}).encode())
 def do_POST(self):raise AssertionError('Inference must never be called by setup')
 def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),API)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
 with tempfile.TemporaryDirectory(prefix='modelcat-buyer-') as temp:
  temp=Path(temp);key=temp/'fixture.key';key.write_text('fixture-only-key')
  env=dict(os.environ,MODELCAT_BUYER_HOME=str(temp/'buyers'),MODELCAT_APPLICATIONS_DIR=str(temp/'Applications'),ASALE_DISABLE_OS_KEYCHAIN_SCAN='1')
  gateway=f'http://127.0.0.1:{server.server_port}'
  for client in ['codex','claude','dsh']:
   subprocess.run([binary,'setup-client','--client',client,'--gateway',gateway,'--key-file',key,'--no-launch'],env=env,check=True,capture_output=True)
  assert len(list((temp/'Applications').glob('*.app')))==3
  profiles=list((temp/'buyers').glob('*/codex/profile'))
  assert len(profiles)==1
  catalog=profiles[0]/'model-catalog.json';assert len(json.loads(catalog.read_text())['models'])==3
  API.models=['fixture-codex-v2','claude-fixture-v2']
  for client in ['codex','claude','dsh']:
   subprocess.run([binary,'setup-client','--client',client,'--gateway',gateway,'--no-launch'],env=env,check=True,capture_output=True)
  assert len(list((temp/'Applications').glob('*.app')))==3,'repeat creates duplicate app'
  assert 'fixture-codex-v2' in catalog.read_text() and 'fixture-dsh' not in catalog.read_text()
  for p in (temp/'buyers').rglob('gateway.key'):assert p.stat().st_mode&0o077==0
  for p in (temp/'buyers').rglob('*.modelcat-backup'):assert p.stat().st_mode&0o077==0
  assert all(p=='/v1/models' for p in API.calls)
  print('Buyer onboarding passed: Codex/Claude Desktop/DSH isolated profiles, three app launchers, repeat model refresh, key reuse, private files; no inference calls.')
finally:server.shutdown()

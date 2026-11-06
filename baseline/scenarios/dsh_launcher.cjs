// Exercise the shipped supervisor with an isolated local DSH stand-in, no model API.
const fs=require('fs'),os=require('os'),path=require('path'),cp=require('child_process'),assert=require('assert');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'modelcat-dsh-launch-'));
const source=path.resolve(__dirname,'../../apps/seller/assets/launch-dsh.mjs');
const wait=ms=>new Promise(r=>setTimeout(r,ms));
const run=(file,env)=>new Promise((resolve,reject)=>{const p=cp.spawn(process.execPath,[file],{env,stdio:'pipe'});p.on('error',reject);p.on('exit',code=>resolve(code))});
(async()=>{
 let manager;
 try{
  const helper=path.join(root,'launch-dsh.mjs');fs.copyFileSync(source,helper);
  const fake=path.join(root,'fake-dsh');fs.writeFileSync(fake,`#!/bin/sh\nexec '${process.execPath}' '${root}/fake.cjs' "$@"\n`,{mode:0o700});
  fs.writeFileSync(path.join(root,'fake.cjs'),`const http=require('http');if(!process.argv.includes('--no-open')||!process.argv.includes('--offline'))process.exit(8);const s=http.createServer((q,r)=>r.end('dsh fixture'));s.listen(0,'127.0.0.1',()=>console.log('dsh web: http://127.0.0.1:'+s.address().port+'/?bootstrap=fixture-only'));`);
  const opener=path.join(root,'open');fs.writeFileSync(opener,`#!/bin/sh\nprintf '%s\\n' "$1" >> '${root}/opened'\n`,{mode:0o700});
  const env={...process.env,MODELCAT_DSH_NPX:fake,MODELCAT_DSH_OPEN:opener,MODELCAT_DSH_WORKDIR:root};
  assert.equal(await run(helper,env),0);const first=JSON.parse(fs.readFileSync(path.join(root,'runtime.json')));manager=first.manager;
  assert.equal(await run(helper,env),0);const second=JSON.parse(fs.readFileSync(path.join(root,'runtime.json')));
  assert.equal(first.manager,second.manager,'second click started another server');assert.equal(first.url,second.url);
  assert.equal(fs.readFileSync(path.join(root,'opened'),'utf8').trim().split('\n').length,2,'second click did not reopen browser');
  assert.equal(fs.statSync(path.join(root,'runtime.json')).mode&0o077,0,'local auth URL is exposed');
  process.kill(manager,'SIGTERM');manager=null;await wait(300);
  fs.writeFileSync(fake,'#!/bin/sh\nexit 7\n',{mode:0o700});
  assert.equal(await run(helper,env),1);assert(fs.existsSync(path.join(root,'启动说明.txt')),'startup error stayed invisible');
  console.log('DSH launcher passed: ready URL, repeat browser reopen, one server, private auth state and visible startup failure.');
 }finally{if(manager)try{process.kill(manager,'SIGTERM')}catch{}await wait(250);fs.rmSync(root,{recursive:true,force:true})}
})().catch(e=>{console.error(e);process.exitCode=1});

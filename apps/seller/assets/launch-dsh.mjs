// ModelCat owns this supervisor only. DSH credentials and conversations stay in its profile.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
const root=path.dirname(fileURLToPath(import.meta.url));
const stateFile=path.join(root,'runtime.json'),lock=path.join(root,'launch.lock');
const alive=pid=>{try{process.kill(pid,0);return true}catch{return false}};
const read=()=>{try{return JSON.parse(fs.readFileSync(stateFile,'utf8'))}catch{return {}}};
function save(value){const tmp=stateFile+'.'+process.pid;fs.writeFileSync(tmp,JSON.stringify(value),{mode:0o600});fs.renameSync(tmp,stateFile)}
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
function localURL(raw){try{const u=new URL(raw);if(u.protocol==='http:'&&['127.0.0.1','localhost','[::1]'].includes(u.hostname)&&u.port)return u}catch{}return null}
async function ready(s){if(s.phase!=='ready'||!alive(s.manager)||!alive(s.child)||!localURL(s.url))return false;try{const r=await fetch(new URL(s.url).origin,{signal:AbortSignal.timeout(1500),redirect:'manual'});return r.status===200||r.status===401}catch{return false}}
function open(url){return new Promise((resolve,reject)=>{const child=spawn(process.env.MODELCAT_DSH_OPEN||'/usr/bin/open',[url],{stdio:'ignore'});child.on('error',reject);child.on('exit',code=>code===0?resolve():reject(Error('无法打开浏览器')));})}
async function server(){
 const child=spawn(process.env.MODELCAT_DSH_NPX||'npx',['--offline','--yes','@deepseek-ai/dsh@0.2.2-rc.1','web','--host','127.0.0.1','--port','0','--no-open'],{cwd:process.env.MODELCAT_DSH_WORKDIR||process.env.HOME,env:process.env,detached:true,stdio:['ignore','pipe','pipe']});
 save({phase:'starting',manager:process.pid,child:child.pid});
 let pending='';
 child.stdout.on('data',buffer=>{
  pending=(pending+buffer.toString()).slice(-32768);
  const lines=pending.split(/\r?\n/);pending=lines.pop();
  for(const line of lines){const match=line.replace(/\u001b\[[0-9;]*m/g,'').match(/^dsh web:\s+(http:\/\/\S+)/);if(read().phase==='starting'&&match&&localURL(match[1]))save({phase:'ready',manager:process.pid,child:child.pid,url:match[1]});}
 });
 // Consume diagnostics without copying provider configuration, keys or prompts into logs.
 child.stderr.on('data',()=>{});
 child.on('error',()=>{save({phase:'failed',manager:process.pid,error:'DSH 未能启动，请重新运行配置安装器检查 Node.js 与网络。'});process.exitCode=1});
 child.on('exit',code=>{if(read().manager===process.pid)save({phase:'failed',manager:process.pid,error:'DSH 服务已退出（'+String(code)+'）。请再次打开 App，或重新运行安装器。'});process.exitCode=code||0});
 for(const signal of ['SIGTERM','SIGINT'])process.on(signal,()=>{try{process.kill(-child.pid,signal)}catch{child.kill(signal)}});
}
async function entry(){
 let ownsLock=false;
 try{
  for(let i=0;i<240;i++){
   const s=read();if(await ready(s)){await open(s.url);return}
   try{fs.mkdirSync(lock,{mode:0o700});fs.writeFileSync(path.join(lock,'owner'),String(process.pid));ownsLock=true;break}catch{
    try{if(!alive(Number(fs.readFileSync(path.join(lock,'owner'),'utf8')))){fs.rmSync(lock,{recursive:true,force:true});continue}}catch{}
    await delay(500);
   }
  }
  if(!ownsLock)throw Error('另一次 DSH 启动尚未完成，请稍后重试。');
  const current=read();
  if(!(current.phase==='starting'&&alive(current.manager))){
   const manager=spawn(process.execPath,[fileURLToPath(import.meta.url),'--server'],{env:process.env,detached:true,stdio:'ignore'});manager.unref();
  }
  for(let i=0;i<240;i++){
   const s=read();if(await ready(s)){await open(s.url);return}
   if(s.phase==='failed'&&s.manager!==current.manager)throw Error(s.error);
   await delay(500);
  }
  throw Error('DSH 启动超时。首次安装需要联网下载，请检查网络后再次打开。');
 }finally{if(ownsLock)fs.rmSync(lock,{recursive:true,force:true})}
}
if(process.argv.includes('--server'))await server();
else try{await entry()}catch{
 const message='ModelCat DSH 暂未启动成功。\n请检查网络后重新打开 App；首次安装可能需要两分钟。\n如果仍失败，请重新下载买家工作台的 DSH 配置安装器。\n';
 const errorFile=path.join(root,'启动说明.txt');fs.writeFileSync(errorFile,message,{mode:0o600});
 await open(errorFile).catch(()=>{});process.exitCode=1;
}

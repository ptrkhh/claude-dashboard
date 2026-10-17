import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

async function fixture(t, failure = '') {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-native-test-'));
  const state = join(dir, 'state.json'), wire = join(dir, 'wire.jsonl');
  await mkdir(join(dir,'controls','cdash-test'),{recursive:true});
  await writeFile(state, JSON.stringify({ dir, native_terminal: true, model: 'gpt-check', effort: 'low', display_name: '[Codex] project · elephant' }));
  await writeFile(wire, '');
  const header = `#!${process.execPath}
const fs=require('node:fs');
const log=m=>fs.appendFileSync(process.env.CDASH_CHECK_WIRE,JSON.stringify(m)+'\\n');
const credential=!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN;
`;
  await writeFile(join(dir, 'codex'), header + `
const args=process.argv.slice(2);
if(args[0]==='app-server'){
 log({kind:'backend',pid:process.pid,args,credential});
 if(process.env.CDASH_CHECK_FAILURE==='backend')process.exit(1);
 const path=args[args.indexOf('--listen')+1].replace('unix://','');
 require('node:net').createServer().listen(path,()=>fs.chmodSync(path,0o600));
}else{
 log({kind:'tui',pid:process.pid,args,credential});
 if(process.env.CDASH_CHECK_FAILURE==='tui')process.exit(1);
 console.log('NATIVE_TUI_READY');
 process.stdin.on('data',b=>{log({kind:'native-input',text:String(b)});if(String(b).includes('quit'))process.exit(0)});
 setInterval(()=>{},1000);
}
`, { mode: 0o700 });
  await writeFile(join(dir, 'agent'), header + `
log({kind:'proxy',pid:process.pid,args:process.argv.slice(2),credential});
setInterval(()=>{
 const marker=process.env.CDASH_CHECK_ROOT+'/native-event';
 if(fs.existsSync(marker)) {
   const events=JSON.parse(fs.readFileSync(marker,'utf8'));fs.unlinkSync(marker);
   for(const event of events)process.stdout.write(JSON.stringify(event)+'\\n');
 }
},30);
require('node:readline').createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);log({rpc:m});
 const send=result=>process.stdout.write(JSON.stringify({id:m.id,result})+'\\n');
 if(m.method==='initialize')send({});
 if(m.method==='account/read')send({account:{type:'apiKey'},requiresOpenaiAuth:false});
 if(['thread/start','thread/resume'].includes(m.method))send({thread:{id:'01234567-1234-1234-1234-0123456789ab'},model:'gpt-check',reasoningEffort:'low'});
 if(m.method==='thread/name/set')send({});
});
`, { mode: 0o700 });
  await writeFile(join(dir, 'claude'), header + `
log({kind:'remote',pid:process.pid});
fs.writeFileSync(process.env.CDASH_CODEX_STATE.replace(/\\.json$/,'' )+'.link','https://claude.ai/code/session_test');
console.log('REMOTE_LOG_ONLY');
setInterval(()=>{if(fs.existsSync(process.env.CDASH_CHECK_ROOT+'/stop-remote'))process.exit(1)},30);
`, { mode: 0o700 });
  const child = spawn(process.execPath, [resolve('crates/agent/src/collect/codex-remote.mjs'), '--state', state, '--name', 'cdash-test'], {
    env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_CHECK_ROOT: dir, CDASH_CHECK_WIRE: wire,
      CDASH_CHECK_FAILURE: failure, CDASH_CODEX_AGENT: join(dir, 'agent'), CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'test-must-not-leak' },
    stdio: ['pipe','pipe','pipe'],
  });
  let output = '';
  child.stdout.on('data',b=>{output+=b;});
  child.stderr.on('data',b=>{output+=b;});
  const messages = async () => (await readFile(wire,'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
  const alive = pid => { try { process.kill(pid,0); return true; } catch { return false; } };
  t.after(async () => {
    child.kill(); child.stdin.end();
    for (const m of await messages()) if (m.pid && alive(m.pid)) process.kill(m.pid,'SIGKILL');
    await rm(dir,{recursive:true,force:true});
  });
  const wait = async check => {
    for(let i=0;i<150;i++) { if(await check())return; await new Promise(r=>setTimeout(r,30)); }
    throw Error(`Native launcher condition timed out: ${output}`);
  };
  return { dir, state, messages, child, wait, alive, output:()=>output };
}

test('native launcher owns one backend, isolates credentials, and preserves TUI after RC exit', {timeout:15000}, async t => {
  const f=await fixture(t);
  await f.wait(()=>f.output().includes('NATIVE_TUI_READY'));
  const messages=await f.messages();
  for(const kind of ['backend','proxy','tui']) {
    assert.equal(messages.filter(m=>m.kind===kind).length,1,kind);
    assert.equal(messages.find(m=>m.kind===kind).credential,false,`${kind} receives no ingress credential`);
  }
  const backend=messages.find(m=>m.kind==='backend'), tui=messages.find(m=>m.kind==='tui');
  assert.equal(tui.args[0],'resume');
  assert.ok(tui.args.includes('tui.fullscreen_transcript=true'),'the TUI must own transcript scrolling');
  assert.ok(tui.args.includes('01234567-1234-1234-1234-0123456789ab'));
  const endpoint=backend.args[backend.args.indexOf('--listen')+1];
  assert.equal(tui.args[tui.args.indexOf('--remote')+1],endpoint);
  const path=endpoint.replace('unix://','');
  assert.equal((await stat(join(path,'..'))).mode & 0o777,0o700);
  assert.ok(messages.some(m=>m.rpc?.method==='thread/name/set' && m.rpc.params.name==='[Codex] project · elephant'));
  assert.equal(f.output().includes('REMOTE_LOG_ONLY'),false,'RC logs cannot overwrite the TUI');
  await f.wait(async()=> (await f.messages()).some(m=>m.kind==='remote'));
  await writeFile(join(f.dir,'stop-remote'),'stop');
  const remote=(await f.messages()).find(m=>m.kind==='remote');
  await f.wait(()=>!f.alive(remote.pid));
  assert.equal(f.alive(tui.pid),true);
  assert.equal(f.alive(backend.pid),true);
  await f.wait(async()=>{try{await stat(f.state.replace(/\.json$/,'')+'.link');return false;}catch{return true;}});
  await writeFile(join(f.dir,'native-event'),JSON.stringify([
    {method:'thread/settings/updated',params:{threadSettings:{model:'gpt-new',effort:'high'}}},
    {method:'turn/started',params:{turn:{id:'offline'}}},
    {method:'item/completed',params:{item:{type:'agentMessage',text:'Offline native answer'}}},
    {method:'turn/completed',params:{turn:{id:'offline',status:'completed'}}},
  ]));
  await f.wait(async()=>{
    const native=JSON.parse(await readFile(f.state.replace(/\.json$/,'.native'),'utf8'));
    return native.model==='gpt-new' && native.effort==='high' && !native.working && native.last_message==='Offline native answer';
  });
  f.child.stdin.write('quit\n');
  await f.wait(()=>f.child.exitCode!==null);
  assert.equal(f.child.exitCode,0);
  await f.wait(async()=> (await f.messages()).filter(m=>m.pid).every(m=>!f.alive(m.pid)));
  await assert.rejects(stat(path),{code:'ENOENT'});
});

for(const failure of ['backend','tui']) test(`native ${failure} failure cleans up its session processes`,{timeout:15000},async t=>{
  const f=await fixture(t,failure);
  await f.wait(()=>f.child.exitCode!==null);
  assert.notEqual(f.child.exitCode,0);
  await f.wait(async()=> (await f.messages()).filter(m=>m.pid).every(m=>!f.alive(m.pid)));
});

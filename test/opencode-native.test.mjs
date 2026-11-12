import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

test('project model query uses a private metadata lease and strips Claude credentials', { timeout: 10000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-native-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const log = join(dir, 'server.json');
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const http = require('node:http'), fs = require('node:fs');
fs.writeFileSync(process.env.CDASH_TEST_LOG, JSON.stringify({args:process.argv.slice(2), leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN, config:JSON.parse(process.env.OPENCODE_CONFIG_CONTENT)}));
const server = http.createServer((req,res) => { if (req.headers.authorization !== 'Basic '+Buffer.from('opencode:'+process.env.OPENCODE_PASSWORD).toString('base64')) { res.writeHead(401); return res.end(); } res.end(JSON.stringify({ data:[{providerID:'fixture',id:'model/one',name:'Project Model',variants:[{id:'deep'}]}] })); });
server.listen(0, '127.0.0.1', () => console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
process.stdin.resume(); process.stdin.on('end',()=>server.close());
`, { mode: 0o700 });
  const result = await new Promise((done, reject) => {
    const child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--models', '--dir', dir], {
      cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'secret' }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', value => stdout += value);
    child.stderr.on('data', value => stderr += value);
    child.once('error', reject);
    child.once('exit', code => code ? reject(Error(stderr)) : done(JSON.parse(stdout)));
  });
  assert.equal(result[0].value, 'opencode:fixture/model/one');
  assert.deepEqual(result[0].variants, ['deep']);
  const server = JSON.parse(await readFile(log, 'utf8'));
  assert.equal(server.leaked, false);
  assert.ok(server.config.plugins.includes('-opencode.config.mcp'));
  assert.equal(server.args.includes('--service'), false);
});

test('history includes only owned active top-level sessions and writes text-only snapshots', { timeout: 10000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-history-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  for (const sid of ['ses_owned', 'ses_archived', 'ses_child']) await writeFile(join(dir, `${sid}.owned.json`), JSON.stringify({ sid, dir, display_name: sid }));
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const http = require('node:http');
const server = http.createServer((req,res) => {
 const parts=req.url.split('?')[0].split('/'), sid=parts[3];
 const data=parts[4]==='message' ? {data:[{id:'msg_a',type:'user',text:'Fix project'}, {id:'msg_b',type:'assistant',content:[{type:'reasoning',text:'private reasoning'},{type:'text',text:'Fixed'}]}],cursor:{}} : {data:{id:sid,title:sid,metadata:{cdash_owner:true},location:{directory:process.env.CDASH_TEST_DIR},time:{updated:1000,...(sid==='ses_archived'?{archived:1}:{})},...(sid==='ses_child'?{parentID:'ses_parent'}:{}),model:{providerID:'fixture',id:'model',variant:'deep'}}};
 res.end(JSON.stringify(data));
});
server.listen(0,'127.0.0.1',()=>console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
process.stdin.resume();process.stdin.on('end',()=>server.close());
`, { mode: 0o700 });
  const rows = await new Promise((done, reject) => {
    const child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--threads', '--root', dir, '--limit', '100'], {
      cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_DIR: dir }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', value => stdout += value); child.stderr.on('data', value => stderr += value);
    child.once('error', reject); child.once('exit', code => { if (code) reject(Error(stderr)); else { try { done(JSON.parse(stdout)); } catch (error) { reject(error); } } });
  });
  assert.deepEqual(rows.map(row => row.sid), ['ses_owned']);
  assert.deepEqual(rows[0].prompts, ['Fix project']);
  assert.equal(rows[0].variant, 'deep');
  const transcript = await readFile(rows[0].source_file, 'utf8');
  assert.match(transcript, /Fix project/);
  assert.match(transcript, /Fixed/);
  assert.doesNotMatch(transcript, /private reasoning/);
});

test('native supervisor attaches one TUI to its owned session and cleans up', { timeout: 15000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-supervisor-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const state = join(dir, 'pane.json'), log = join(dir, 'wire.jsonl');
  await writeFile(state, JSON.stringify({ dir, display_name: 'Fixture Session', sid: null, model: 'fixture/model', model_label: 'Old Model', variant: 'deep' }));
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const fs=require('node:fs'), http=require('node:http');
const log=x=>fs.appendFileSync(process.env.CDASH_TEST_LOG,JSON.stringify(x)+'\\n');
if(process.argv.includes('--server')) {log({kind:'tui',pid:process.pid,args:process.argv.slice(2),hasPassword:!!process.env.OPENCODE_PASSWORD,leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN}); process.stdin.resume();setInterval(()=>{},1000);}
else {
 log({kind:'server',pid:process.pid,leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN});
 let eventConnections=0;
 const server=http.createServer(async(req,res)=>{let raw='';for await(const c of req)raw+=c;log({kind:'api',method:req.method,path:req.url,body:raw?JSON.parse(raw):null});
 if(req.url==='/api/event'){res.writeHead(200,{'content-type':'text/event-stream'});res.write('data: {"type":"server.connected","data":{}}\\n\\n');if(++eventConnections===1)setTimeout(()=>res.end(),50);else setTimeout(()=>res.write('data: '+JSON.stringify({type:'session.model.selected',data:{sessionID:'ses_fixture',model:{providerID:'fixture',id:'new-model'}}})+'\\n\\n'),500);return;}
 if(req.url==='/api/session/active')return res.end(JSON.stringify({data:eventConnections>1?{ses_fixture:{type:'running'}}:{}}));
 if(req.url==='/api/session'&&req.method==='POST')return res.end(JSON.stringify({data:{id:'ses_fixture'}}));
 if(req.url==='/api/session/ses_fixture/model'){res.writeHead(204);return res.end();}
 return res.end(JSON.stringify({data:{id:'ses_fixture',metadata:{cdash_owner:true},time:{},model:{id:'model',providerID:'fixture',variant:'deep'}}}));});
 server.listen(0,'127.0.0.1',()=>console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
 process.stdin.resume();process.stdin.on('end',()=>server.closeAllConnections()||server.close());
}
`, { mode: 0o700 });
  await writeFile(join(dir, 'claude'), `#!${process.execPath}\nconst child=require('node:child_process').spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});require('node:fs').appendFileSync(process.env.CDASH_TEST_LOG,JSON.stringify({kind:'claude',descendant:child.pid})+'\\n');process.exit(1);`, { mode: 0o700 });
  let child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--state', state, '--name', 'cdash-fixture'], {
    cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'do-not-leak' }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stderr = ''; child.stderr.on('data', data => stderr += data);
  let rows = [];
  try {
    for (let i=0;i<100;i++) {
      rows = (await readFile(log, 'utf8').catch(() => '')).trim().split('\n').filter(Boolean).map(JSON.parse);
      if (rows.some(row => row.kind === 'tui')) break;
      if (child.exitCode !== null) throw Error(`supervisor exited: ${stderr}`);
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    const tui = rows.find(row => row.kind === 'tui');
    assert.ok(tui, stderr);
    assert.equal(rows.filter(row => row.kind === 'server').length, 1);
    assert.equal(tui.args[tui.args.indexOf('--session')+1], 'ses_fixture');
    assert.ok(tui.args.includes('--auto'), 'fresh coding sessions must auto-approve without saved config');
    assert.equal(tui.hasPassword, true);
    assert.equal(tui.leaked, false);
    assert.equal(rows.find(row => row.kind === 'server').leaked, false);
    assert.equal(JSON.parse(await readFile(join(dir, 'ses_fixture.owned.json'), 'utf8')).sid, 'ses_fixture');
    assert.equal(JSON.parse(await readFile(join(dir, 'pane.native'), 'utf8')).sid, 'ses_fixture');
    for (let i = 0; i < 60; i++) {
      rows = (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
      if (rows.filter(row => row.kind === 'claude').length >= 2) break;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.ok(rows.filter(row => row.kind === 'claude').length >= 2, 'Remote Control should retry');
    assert.equal(child.exitCode, null, 'native supervisor remains alive during Remote Control failures');
    let nativeState;
    for (let i = 0; i < 100; i++) {
      nativeState = JSON.parse(await readFile(join(dir, 'pane.native'), 'utf8'));
      if (nativeState.working) break;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.equal(nativeState.working, true, 'native status must recover after event stream disconnect');
    for (let i = 0; i < 100 && nativeState.model !== 'fixture/new-model'; i++) {
      await new Promise(resolve => setTimeout(resolve, 50));
      nativeState = JSON.parse(await readFile(join(dir, 'pane.native'), 'utf8'));
    }
    assert.equal(nativeState.model, 'fixture/new-model');
    assert.equal(nativeState.model_label, null, 'a native model switch must not keep the old label');
    const orphan = rows.find(row => row.kind === 'claude').descendant;
    try { assert.throws(() => process.kill(orphan, 0), { code: 'ESRCH' }, 'a failed runner must not leave children'); }
    finally { try { process.kill(orphan, 'SIGKILL'); } catch {} }
    const firstExit = new Promise(resolve => child.once('exit', resolve));
    child.kill('SIGTERM'); await firstExit;
    child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--state', state, '--name', 'cdash-fixture'], {
      cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'do-not-leak' }, stdio: ['pipe', 'pipe', 'pipe'],
    });
    child.stderr.on('data', data => stderr += data);
    for (let i = 0; i < 100; i++) {
      rows = (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
      if (rows.filter(row => row.kind === 'tui').length === 2) break;
      if (child.exitCode !== null) throw Error(`resume exited: ${stderr}`);
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    const resumed = rows.filter(row => row.kind === 'tui').at(-1);
    assert.equal(rows.filter(row => row.kind === 'tui').length, 2);
    assert.equal(resumed.args[resumed.args.indexOf('--session') + 1], 'ses_fixture');
    assert.ok(resumed.args.includes('--auto'), 'resumed coding sessions must also auto-approve');
    assert.equal(rows.filter(row => row.kind === 'api' && row.path === '/api/session' && row.method === 'POST').length, 1, 'resume must not create another session');
    assert.equal(rows.filter(row => row.kind === 'api' && row.path === '/api/session/ses_fixture/model').length, 0, 'unchanged model must not be reselected');
  } finally {
    if (child.exitCode === null) { const exit = new Promise(resolve => child.once('exit', resolve)); child.kill('SIGTERM'); await exit; }
    for (const row of rows.filter(item => item.descendant)) { try { process.kill(row.descendant, 'SIGKILL'); } catch {} }
    for (const row of rows.filter(item => item.pid)) {
      let alive = true;
      for (let i = 0; i < 50 && alive; i++) {
        try { process.kill(row.pid, 0); await new Promise(resolve => setTimeout(resolve, 20)); }
        catch (error) { if (error.code === 'ESRCH') alive = false; else throw error; }
      }
      assert.equal(alive, false, `${row.kind} process leaked`);
    }
  }
});

// A supervisor over a fake OpenCode server that also serves the chat feed's
// reads (messages, forms, permissions) and records prompts and interrupts.
async function fixture(t, { flakyMessages = false } = {}) {
  const { readFileSync, existsSync } = await import('node:fs');
  const { rename } = await import('node:fs/promises');
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-chat-'));
  const state = join(dir, 'pane.json'), log = join(dir, 'wire.jsonl'), marker = join(dir, 'emit-event');
  await writeFile(log, '');
  await writeFile(state, JSON.stringify({ dir, display_name: 'Chat Session', sid: null, model: 'fixture/model', variant: null }));
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const fs=require('node:fs'), http=require('node:http');
const log=x=>fs.appendFileSync(process.env.CDASH_TEST_LOG,JSON.stringify(x)+'\\n');
if(process.argv.includes('--server')) {log({kind:'tui',pid:process.pid}); process.stdin.resume();setInterval(()=>{},1000);}
else {
 log({kind:'server',pid:process.pid});
 const streams=new Set();
 setInterval(()=>{const m=process.env.CDASH_TEST_MARKER;if(fs.existsSync(m)){const e=fs.readFileSync(m,'utf8');fs.unlinkSync(m);for(const s of streams)s.write('data: '+e+'\\n\\n');}},30);
 const send=(res,data)=>res.end(JSON.stringify(data));
 const server=http.createServer(async(req,res)=>{let raw='';for await(const c of req)raw+=c;const body=raw?JSON.parse(raw):null;const path=req.url.split('?')[0];log({kind:'api',method:req.method,path,body});
 if(path==='/api/event'){res.writeHead(200,{'content-type':'text/event-stream'});res.write('data: {"type":"server.connected","data":{}}\\n\\n');streams.add(res);req.on('close',()=>streams.delete(res));return;}
 if(path==='/api/session/active')return send(res,{data:{}});
 if(path==='/api/session'&&req.method==='POST')return send(res,{data:{id:'ses_fixture'}});
 if(path==='/api/session/ses_fixture/message'&&process.env.CDASH_TEST_FLAKY&&!fs.existsSync(process.env.CDASH_TEST_FLAKY)){fs.writeFileSync(process.env.CDASH_TEST_FLAKY,'1');return send(res,{data:'broken'});}
 if(path==='/api/session/ses_fixture/message')return send(res,{data:[{id:'msg_u',type:'user',text:'hello'},{id:'msg_a',type:'assistant',time:{completed:1},content:[{type:'text',text:'hi there'}]}],cursor:{}});
 if(path.endsWith('/form')||path.endsWith('/permission'))return send(res,{data:[]});
 if(path.endsWith('/prompt'))return send(res,{data:{id:body.id,sessionID:'ses_fixture',type:'user'}});
 if(path.endsWith('/interrupt'))return send(res,{data:true});
 return send(res,{data:{id:'ses_fixture',metadata:{cdash_owner:true},time:{},model:{id:'model',providerID:'fixture'}}});});
 server.listen(0,'127.0.0.1',()=>console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
 process.stdin.resume();process.stdin.on('end',()=>server.closeAllConnections()||server.close());
}
`, { mode: 0o700 });
  await writeFile(join(dir, 'claude'), `#!${process.execPath}\nprocess.exit(1);`, { mode: 0o700 });
  const child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--state', state, '--name', 'cdash-fixture'], {
    cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CDASH_TEST_MARKER: marker, ...(flakyMessages && { CDASH_TEST_FLAKY: join(dir, 'failed-once') }) }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  let output = '';
  child.stdout.on('data', b => { output += b; });
  child.stderr.on('data', b => { output += b; });
  const rows = () => readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  t.after(async () => {
    if (child.exitCode === null) { const exit = new Promise(r => child.once('exit', r)); child.kill('SIGTERM'); await exit; }
    for (const row of rows().filter(r => r.pid)) { try { process.kill(row.pid, 'SIGKILL'); } catch {} }
    await rm(dir, { recursive: true, force: true });
  });
  const wait = async check => {
    for (let i = 0; i < 200; i++) { if (await check()) return; if (child.exitCode !== null) throw Error(`supervisor exited: ${output}`); await new Promise(r => setTimeout(r, 30)); }
    throw Error(`condition timed out: ${output}`);
  };
  return {
    dir, wait, output: () => output,
    native: () => { try { return JSON.parse(readFileSync(join(dir, 'pane.native'), 'utf8')); } catch { return undefined; } },
    posted: suffix => rows().filter(r => r.kind === 'api' && r.method === 'POST' && r.path.endsWith(suffix)).at(-1)?.body,
    emit: async event => {
      await wait(() => !existsSync(marker));
      await writeFile(marker + '.tmp', JSON.stringify(event));
      await rename(marker + '.tmp', marker);
    },
  };
}

async function client(path, first) {
  const { createConnection } = await import('node:net');
  const socket = createConnection(path); const lines = []; let buf = '';
  socket.on('data', d => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { lines.push(JSON.parse(buf.slice(0, i))); buf = buf.slice(i + 1); } });
  await new Promise(r => socket.once('connect', r));
  socket.write(JSON.stringify(first) + '\n');
  return { lines, write: o => socket.write(JSON.stringify(o) + '\n'), close: () => socket.destroy() };
}
const until = async (f, ms = 5000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw Error('timed out'); await new Promise(r => setTimeout(r, 10)); } };
test('the supervisor serves chat: history, send as steering, and stop', { timeout: 20000 }, async t => {
  const f = await fixture(t);                                     // the file's existing supervisor fixture
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const evs = c.lines.filter(l => l.t === 'ev').map(l => l.ev);
  assert.deepEqual(evs.map(e => [e.type, e.message.content[0].text]), [['user', 'hello'], ['assistant', 'hi there']]);
  c.write({ t: 'send', cid: 'c1', text: 'next', steer: true });
  await until(() => f.posted('/prompt'));
  assert.deepEqual([f.posted('/prompt').text, f.posted('/prompt').delivery], ['next', 'steer']);
  assert.deepEqual(c.lines.find(l => l.t === 'ack'), { t: 'ack', cid: 'c1', ok: true });
  c.write({ t: 'stop' });
  await until(() => f.posted('/interrupt'));
  await f.emit({ type: 'session.execution.started', data: { sessionID: f.native().sid } });
  await until(() => c.lines.some(l => l.t === 'state' && l.working === true));
  c.close();
  assert.ok(!String(f.output()).includes('"t":"'), 'no feed frame leaked onto the TUI');
});

test('one failed history read neither leaves chat connecting forever nor freezes it', { timeout: 20000 }, async t => {
  const f = await fixture(t, { flakyMessages: true });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await until(() => c.lines.filter(l => l.t === 'ev').length >= 2, 10000);
  assert.deepEqual(c.lines.filter(l => l.t === 'ev').map(l => l.ev.message.content[0].text), ['hello', 'hi there']);
  c.close();
});

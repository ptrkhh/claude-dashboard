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
  await writeFile(state, JSON.stringify({ dir, display_name: 'Fixture Session', sid: null, model: 'fixture/model', variant: 'deep' }));
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const fs=require('node:fs'), http=require('node:http');
const log=x=>fs.appendFileSync(process.env.CDASH_TEST_LOG,JSON.stringify(x)+'\\n');
if(process.argv.includes('--server')) {log({kind:'tui',pid:process.pid,args:process.argv.slice(2),hasPassword:!!process.env.OPENCODE_PASSWORD,leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN}); process.stdin.resume();setInterval(()=>{},1000);}
else {
 log({kind:'server',pid:process.pid,leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN});
 let eventConnections=0;
 const server=http.createServer(async(req,res)=>{let raw='';for await(const c of req)raw+=c;log({kind:'api',method:req.method,path:req.url,body:raw?JSON.parse(raw):null});
 if(req.url==='/api/event'){res.writeHead(200,{'content-type':'text/event-stream'});res.write('data: {"type":"server.connected","data":{}}\\n\\n');if(++eventConnections===1)setTimeout(()=>res.end(),50);return;}
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

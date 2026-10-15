import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createConnection } from 'node:net';
import { createHash } from 'node:crypto';

// Exercises the real worker across process, RPC, SSE, and filesystem boundaries.
test('Codex bridge dispatches string SSE sequences, persists resume, rotates credentials, and denies unsupported requests', { timeout: 25_000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-codex-check-'));
  await mkdir(join(dir, 'history'));
  const state = join(dir, 'session.json'), wire = join(dir, 'wire.jsonl');
  await writeFile(state, JSON.stringify({ dir, sid: null, effort: 'low' }));
  await writeFile(wire, '');
  const preload = join(dir, 'transport.mjs');
  await writeFile(preload, `
import {appendFileSync} from 'node:fs';
const log = m => appendFileSync(process.env.CDASH_CHECK_WIRE, JSON.stringify(m)+'\\n');
let inbound;
globalThis.fetch = async (url, options) => {
  log({url,method:options.method,token:options.headers.Authorization,body:options.body && JSON.parse(options.body)});
  if (!url.includes('/stream')) {
    const payload=options.body && JSON.parse(options.body).events?.find(e=>e.payload.type==='user')?.payload;
    if(payload && inbound) inbound.enqueue(new TextEncoder().encode('event: client_event\\ndata: '+JSON.stringify({sequence_num:'12',event_id:'local-echo',payload})+'\\n\\n'));
    return new Response('{}');
  }
  const events = [
    {sequence_num:'1',event_id:'init',payload:{type:'control_request',request_id:'init-req',request:{subtype:'initialize'}}},
    {sequence_num:'2',event_id:'first',payload:{type:'user',message:{content:'first'}}},
    {sequence_num:'11',event_id:'second',payload:{type:'user',message:{content:'second'}}},
  ];
  return new Response(new ReadableStream({start(c){
    inbound=c;
    const text = events.map(e=>'event: client_event\\r\\ndata: '+JSON.stringify(e)+'\\r\\n\\r\\n').join('');
    const bytes=new TextEncoder().encode(text);
    for(let i=0;i<bytes.length;i+=7)c.enqueue(bytes.slice(i,i+7));
    options.signal.addEventListener('abort',()=>c.close(),{once:true});
  }}));
};
`);
  await writeFile(join(dir, 'codex'), `#!/usr/bin/env node
const {createInterface}=require('node:readline');
const {appendFileSync}=require('node:fs');
const send=m=>process.stdout.write(JSON.stringify(m)+'\\n');
createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);
 appendFileSync(process.env.CDASH_CHECK_WIRE,JSON.stringify({rpc:m,credentialLeaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN})+'\\n');
 if(m.method==='initialize')send({id:m.id,result:{}});
 if(m.method==='model/list')send({id:m.id,result:{data:[{model:'gpt-check',displayName:'GPT Check',supportedReasoningEfforts:[{reasoningEffort:'low'}],defaultReasoningEffort:'low'}],nextCursor:null}});
 if(['thread/start','thread/resume'].includes(m.method))send({id:m.id,result:{thread:{id:'01234567-1234-1234-1234-0123456789ab'},model:'gpt-check',reasoningEffort:'low'}});
 if(m.method==='turn/start'){
   const text=m.params.input[0].text;
   send({id:m.id,result:{turn:{id:text}}});
   send({method:'turn/started',params:{turn:{id:text}}});
   send({method:'item/completed',params:{item:{type:'agentMessage',id:text,text:'Reply '+text}}});
   send({id:500,method:'mcpServer/elicitation/request',params:{}});
   send({method:'turn/completed',params:{turn:{id:text,status:'completed'}}});
 }
 if(m.method==='turn/steer')send({id:m.id,result:{turnId:'first'}});
});
`, { mode: 0o700 });
  const messages = async () => (await readFile(wire, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
  const waitFor = async condition => {
    const deadline = Date.now() + 10_000;
    while (!await condition()) {
      if (Date.now() > deadline) throw Error('Worker condition timed out');
      await new Promise(r => setTimeout(r, 25));
    }
  };
  let child, output = '';
  const start = () => {
    child = spawn(process.execPath, [resolve('crates/agent/src/collect/codex-remote.mjs'), '--worker', 'claude', '--sdk-url', 'https://api.anthropic.com/v1/code/sessions/cse_check', '--session-id', 'cse_check'], {
      env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, NODE_OPTIONS: `--import=${pathToFileURL(preload)}`, CDASH_CHECK_WIRE: wire, CDASH_CODEX_STATE: state, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'test_ingress', CLAUDE_CODE_WORKER_EPOCH: '1' },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    child.stdout.on('data', b => { output += b; });
    child.stderr.on('data', b => { output += b; });
  };
  const stop = async () => {
    if (!child || child.exitCode !== null) return;
    const done = new Promise(r => child.once('exit', r));
    child.kill('SIGTERM'); child.stdin.end(); await done;
  };
  t.after(async () => { await stop(); await rm(dir, { recursive: true, force: true }); });
  start();
  await waitFor(async () => (await messages()).some(m => m.body?.updates?.some(u => u.event_id === 'second' && u.status === 'processed')));
  const first = await messages();
  assert.equal(first.some(m => m.credentialLeaked), false);
  assert.equal(first.filter(m => m.rpc?.method === 'turn/start' || m.rpc?.method === 'turn/steer').length, 2);
  assert.ok(first.some(m => m.rpc?.id === 500 && m.rpc.error?.code === -32601));
  assert.ok(first.some(m => m.body?.events?.some(e => e.payload.type === 'assistant')));
  assert.equal(JSON.parse(await readFile(state, 'utf8')).remote_sequence, 11);
  child.stdin.write(JSON.stringify({type:'update_environment_variables',variables:{CLAUDE_CODE_SESSION_ACCESS_TOKEN:'rotated'}})+'\n');
  const socket = createConnection(join(tmpdir(), `cdash-codex-${process.getuid()}-${createHash('sha256').update(state).digest('hex').slice(0,16)}`, 'input.sock'));
  await new Promise((r,j) => { socket.once('error',j); socket.once('connect',()=>socket.end('third',r)); });
  await waitFor(async () => (await messages()).some(m => m.token === 'Bearer rotated' && m.body?.events?.some(e => e.payload.type === 'user')));
  await waitFor(async () => JSON.parse(await readFile(state, 'utf8')).remote_sequence === 12);
  await waitFor(async () => { const s=JSON.parse(await readFile(state, 'utf8')); return s.last_message === 'Reply third' && !s.working; });
  assert.equal((await messages()).filter(m => ['turn/start','turn/steer'].includes(m.rpc?.method)).length, 3, 'an echoed local prompt must execute once');
  await stop();
  const before = (await messages()).length;
  output = ''; start();
  await waitFor(() => output.includes(' ready'));
  const after = (await messages()).slice(before);
  assert.ok(after.some(m => m.rpc?.method === 'thread/resume'));
  assert.ok(after.some(m => m.url?.includes('from_sequence_num=12')));
  assert.equal(after.some(m => ['turn/start', 'turn/steer'].includes(m.rpc?.method)), false, 'replayed prompts must not execute again');
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createConnection } from 'node:net';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

test('Codex launcher uses the saved animal name even when display names repeat', { timeout: 10_000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-codex-name-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  await writeFile(join(dir, 'claude'), `#!${process.execPath}
require('node:fs').writeFileSync(process.env.CDASH_CHECK_ARGS, JSON.stringify(process.argv.slice(2)));
`, { mode: 0o700 });
  const state = join(dir, 'state.json'), argsPath = join(dir, 'args.json');
  const display_name = '[Codex] claude-dashboard · elephant';
  for (const name of ['cdash-first', 'cdash-second']) {
    await mkdir(join(dir, 'controls', name), { recursive: true });
    await writeFile(state, JSON.stringify({ dir, display_name, model_label: 'GPT Check' }));
    await new Promise((done, reject) => {
      const child = spawn(process.execPath, [resolve('crates/agent/src/collect/codex-remote.mjs'), '--state', state, '--name', name], {
        env: { ...process.env, PATH: dir, CDASH_CHECK_ARGS: argsPath },
        stdio: 'ignore',
      });
      child.on('error', reject);
      child.on('exit', code => code ? reject(Error(`Launcher exited ${code}`)) : done());
    });
    const args = JSON.parse(await readFile(argsPath, 'utf8'));
    assert.equal(args[args.indexOf('--name') + 1], display_name);
  }
});

// Exercises the real worker across process, RPC, SSE, and filesystem boundaries.
test('Codex bridge dispatches string SSE sequences, persists resume, rotates credentials, and denies unsupported requests', { timeout: 25_000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-codex-check-'));
  const state = join(dir, 'session.json'), wire = join(dir, 'wire.jsonl');
  await writeFile(state, JSON.stringify({ dir, sid: null, effort: 'low', display_name: '[Codex] project · elephant' }));
  await writeFile(wire, '');
  const preload = join(dir, 'transport.mjs');
  await writeFile(preload, `
import {appendFileSync} from 'node:fs';
const log = m => appendFileSync(process.env.CDASH_CHECK_WIRE, JSON.stringify(m)+'\\n');
let inbound;
const echoes = [];
globalThis.fetch = async (url, options) => {
  log({url,method:options.method,token:options.headers.Authorization,body:options.body && JSON.parse(options.body)});
  if (!url.includes('/stream')) {
    if(process.env.CDASH_CHECK_FAIL_HISTORY && JSON.parse(options.body || '{}').events?.some(e=>e.payload.message?.id==='old-reply-1')) return new Response('',{status:400});
    if(JSON.parse(options.body || '{}').events?.some(e=>process.env.CDASH_CHECK_RACE==='init' ? e.payload.subtype==='init' : process.env.CDASH_CHECK_RACE==='replay' && e.payload.message?.id==='offline-user')) await new Promise(r=>setTimeout(r,150));
    const payload=options.body && JSON.parse(options.body).events?.find(e=>e.payload.type==='user')?.payload;
    if(payload && inbound) inbound.enqueue(new TextEncoder().encode('event: client_event\\ndata: '+JSON.stringify({sequence_num:'12',event_id:'local-echo',payload})+'\\n\\n'));
    else if(payload) echoes.push(payload);
    return new Response('{}');
  }
  const events = url.includes('/cse_check/') ? [
    {sequence_num:'1',event_id:'init',payload:{type:'control_request',request_id:'init-req',request:{subtype:'initialize'}}},
    {sequence_num:'2',event_id:'first',payload:{type:'user',message:{content:'first'}}},
    {sequence_num:'11',event_id:'second',payload:{type:'user',message:{content:'second'}}},
  ] : echoes.map((payload, i) => ({sequence_num:String(i+1),event_id:'history-'+i,source:'worker',payload}));
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
let experimental=false;
createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);
 appendFileSync(process.env.CDASH_CHECK_WIRE,JSON.stringify({rpc:m,credentialLeaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN})+'\\n');
 if(m.method==='initialize'){experimental=!!m.params.capabilities?.experimentalApi;send({id:m.id,result:{}});}
 if(m.params?.initialTurnsPage && !experimental){send({id:m.id,error:{code:-32600,message:'initialTurnsPage requires experimentalApi'}});return;}
 if(m.method==='account/read')send({id:m.id,result:{account:{type:'apiKey'},requiresOpenaiAuth:true}});
 if(m.method==='model/list')send({id:m.id,result:{data:[{model:'gpt-check',displayName:'GPT Check',supportedReasoningEfforts:[{reasoningEffort:'low'}],defaultReasoningEffort:'low'}],nextCursor:null}});
 if(['thread/start','thread/resume'].includes(m.method))send({id:m.id,result:{
   thread:{id:'01234567-1234-1234-1234-0123456789ab',status:{type:process.env.CDASH_CHECK_NATIVE ? 'active' : 'idle'}},model:'gpt-check',reasoningEffort:'low',
   ...(process.env.CDASH_CHECK_NATIVE && m.params.initialTurnsPage ? {initialTurnsPage:{data:[{id:'native-active',status:'inProgress',items:[]}],nextCursor:null}} : {}),
 }});
 if(m.method==='thread/resume' && process.env.CDASH_CHECK_RACE==='init')setTimeout(()=>send({method:'turn/completed',params:{turn:{id:'native-active',status:'completed'}}}),20);
 if(m.method==='thread/turns/list' && process.env.CDASH_CHECK_NATIVE){
   send({id:m.id,result:{data:[{id:'offline-turn',status:'completed',items:[{type:'userMessage',id:'offline-user',clientId:'native-offline',content:[{type:'text',text:'While bridge offline'}]},{type:'userMessage',id:'own-user',clientId:'own-remote-uuid',content:[{type:'text',text:'Own remote question'}]},{type:'agentMessage',id:'offline-reply',text:'Offline answer'}]}],nextCursor:null}});
   if(process.env.CDASH_CHECK_RACE==='replay')setTimeout(()=>send({method:'turn/completed',params:{turn:{id:'native-active',status:'completed'}}}),20);
   return;
 }
 if(m.method==='thread/turns/list')send({id:m.id,result:m.params.cursor ? {
   data:[{id:'old-turn-2',status:'completed',items:[
     {type:'reasoning',id:'private-reasoning',summary:['private reasoning'],content:[]},
     {type:'commandExecution',id:'old-command',command:'echo history',cwd:'/work',status:'completed',aggregatedOutput:'history output',exitCode:0},
     {type:'agentMessage',id:'old-reply-2',text:'Earlier final answer',phase:'final_answer'},
   ]}],nextCursor:null,
 } : {
   data:[{id:'old-turn-1',status:'completed',items:[
     {type:'userMessage',id:'old-user',content:[{type:'text',text:'Earlier question',text_elements:[]},{type:'text',text:'More context',text_elements:[]}]},
     {type:'agentMessage',id:'old-reply-1',text:'Earlier progress',phase:'commentary'},
   ]}],nextCursor:'next-turn',
 }});
 if(m.method==='turn/start'){
   const text=m.params.input[0].text;
   send({id:m.id,result:{turn:{id:text}}});
   send({method:'turn/started',params:{turn:{id:text}}});
   send({method:'item/completed',params:{turnId:text,item:{type:'userMessage',id:'user-'+text,clientId:m.params.clientUserMessageId,content:[{type:'text',text}]}}});
   if(text==='third'){
     send({method:'item/completed',params:{turnId:'native-turn',item:{type:'userMessage',id:'native-user',clientId:'22222222-2222-4222-8222-222222222222',content:[{type:'text',text:'Native prompt'}]}}});
     send({id:501,method:'item/commandExecution/requestApproval',params:{itemId:'approval-third',command:'true'}});
     send({method:'serverRequest/resolved',params:{requestId:501}});
   }
   const finish=()=>{
     send({method:'item/completed',params:{turnId:text,item:{type:'agentMessage',id:text,text:'Reply '+text,phase:'final_answer'}}});
     send({id:500,method:'mcpServer/elicitation/request',params:{}});
     send({method:'turn/completed',params:{turn:{id:text,status:'completed'}}});
   };
   if(text==='third'){
     send({method:'item/completed',params:{turnId:text,item:{type:'agentMessage',id:'third-progress',text:'Checking Finku',phase:'commentary'}}});
     const tools=[
       {type:'mcpToolCall',id:'beeper-search',server:'beeper',tool:'search_chats',arguments:{query:'Finku'},status:'inProgress',result:null,error:null},
       {type:'commandExecution',id:'shell-check',command:'pwd',cwd:'/work',status:'inProgress',aggregatedOutput:null,exitCode:null},
       {type:'dynamicToolCall',id:'dynamic-check',namespace:'functions',tool:'lookup',arguments:'lookup id 1',status:'inProgress',contentItems:null,success:null},
       {type:'fileChange',id:'file-check',changes:[{path:'/work/check.txt',kind:{type:'add'},diff:'+check'}],status:'inProgress'},
     ];
     for(const item of tools)send({method:'item/started',params:{turnId:text,item}});
     setTimeout(()=>{
       appendFileSync(process.env.CDASH_CHECK_WIRE,JSON.stringify({toolsCompleted:true})+'\\n');
       const results=[{result:{content:[{type:'text',text:'Found Finku'+'x'.repeat(70_000)}],structuredContent:null,_meta:{private:'do not forward'}}},
         {aggregatedOutput:'/work',exitCode:0},{contentItems:[{type:'inputText',text:'Lookup failed'}],success:false},{}];
       tools.forEach((item,i)=>send({method:'item/completed',params:{turnId:text,item:{...item,...results[i],status:i===2?'failed':'completed'}}}));
       // A worker can first observe a tool at completion after reconnecting.
       send({method:'item/completed',params:{turnId:text,item:{type:'mcpToolCall',id:'missed-start',server:'beeper',tool:'get_chat',arguments:{id:'chat'},status:'failed',result:null,error:{message:'Chat unavailable'}}}});
       send({method:'item/completed',params:{turnId:text,item:{type:'reasoning',id:'private-thought',summary:['private reasoning'],content:[]}}});
       finish();
     },300);
   }else finish();
 }
 if(m.method==='turn/steer'){
   send({id:m.id,result:{turnId:'native-active'}});
   if(process.env.CDASH_CHECK_NATIVE){
     send({id:500,method:'mcpServer/elicitation/request',params:{}});
     send({method:'item/completed',params:{turnId:'native-active',item:{type:'agentMessage',id:'native-reply',text:'Reply third'}}});
     send({method:'turn/completed',params:{turn:{id:'native-active',status:'completed'}}});
   }
 }
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
  const start = (sid = 'cse_check', failHistory = false, native = false, race = '') => {
    child = spawn(process.execPath, [resolve('crates/agent/src/collect/codex-remote.mjs'), '--worker', 'claude', '--sdk-url', 'https://api.anthropic.com/v1/code/sessions/'+sid, '--session-id', sid], {
      env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, NODE_OPTIONS: `--import=${pathToFileURL(preload)}`, CDASH_CHECK_WIRE: wire, CDASH_CHECK_NATIVE: native ? '1' : '', CDASH_CHECK_FAIL_HISTORY: failHistory ? '1' : '', CDASH_CHECK_RACE: race, CDASH_CODEX_STATE: state, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'test_ingress', CLAUDE_CODE_WORKER_EPOCH: '1' },
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
  assert.equal(await readFile(join(dir, '01234567-1234-1234-1234-0123456789ab.name'), 'utf8'), '[Codex] project · elephant');
  assert.deepEqual((await readdir(dir)).filter(f => f === 'history'), [], 'Codex owns thread history; the bridge keeps no copy');
  child.stdin.write(JSON.stringify({type:'update_environment_variables',variables:{CLAUDE_CODE_SESSION_ACCESS_TOKEN:'rotated'}})+'\n');
  const socket = createConnection(join(tmpdir(), `cdash-codex-${process.getuid()}-${createHash('sha256').update(state).digest('hex').slice(0,16)}`, 'input.sock'));
  await new Promise((r,j) => { socket.once('error',j); socket.once('connect',()=>socket.end('third',r)); });
  await waitFor(async () => (await messages()).some(m => m.token === 'Bearer rotated' && m.body?.events?.some(e => e.payload.type === 'user')));
  await waitFor(async () => JSON.parse(await readFile(state, 'utf8')).remote_sequence === 12);
  await waitFor(async () => { const s=JSON.parse(await readFile(state, 'utf8')); return s.last_message === 'Reply third' && !s.working; });
  assert.equal((await messages()).filter(m => ['turn/start','turn/steer'].includes(m.rpc?.method)).length, 3, 'an echoed local prompt must execute once');
  const live = (await messages()).flatMap(m => m.body?.events || []).map(e => e.payload);
  const toolUses = live.flatMap(p => p.type === 'assistant' ? p.message?.content || [] : []).filter(b => b.type === 'tool_use');
  assert.deepEqual(toolUses.map(b => [b.id,b.name]), [
    ['beeper-search','mcp__beeper__search_chats'],['shell-check','Bash'],
    ['dynamic-check','functions.lookup'],['file-check','Edit'],['missed-start','mcp__beeper__get_chat'],
  ], 'remote control must receive tool names at start, including tools first seen at completion');
  assert.deepEqual(toolUses[0].input,{query:'Finku'});
  assert.deepEqual(toolUses[2].input,{input:'lookup id 1'},'scalar tool arguments must remain valid Claude tool input');
  const toolResults = live.flatMap(p => p.type === 'user' ? p.message?.content || [] : []).filter(b => b.type === 'tool_result');
  assert.deepEqual(toolResults.map(b => [b.tool_use_id,b.is_error]), [
    ['beeper-search',false],['shell-check',false],['dynamic-check',true],['file-check',false],['missed-start',true],
  ]);
  assert.ok(toolResults[0].content.startsWith('Found Finku'));
  assert.equal(toolResults[0].content.length,65_536,'large tool output must not overwhelm the remote transport');
  assert.equal(toolResults[1].content,'/work');
  assert.equal(toolResults[2].content,'Lookup failed');
  assert.match(toolResults[4].content,/Chat unavailable/);
  assert.equal(JSON.stringify(live).includes('do not forward'),false,'MCP metadata must stay private');
  assert.equal(JSON.stringify(live).includes('private reasoning'),false);
  const toolStartIndex=(await messages()).findIndex(m=>m.body?.events?.some(e=>e.payload.message?.content?.some?.(b=>b.id==='beeper-search')));
  assert.ok(toolStartIndex>=0 && toolStartIndex<(await messages()).findIndex(m=>m.toolsCompleted), 'tool starts must upload while execution is still pending');
  const activity=live.filter(p=>p.type==='stream_event');
  assert.equal(activity[0]?.event.type,'message_start','a turn needs a visible activity signal');
  assert.equal(activity.at(-1)?.event.type,'message_stop','turn completion must stop activity');
  const commentaryIndex=live.findIndex(p=>p.message?.id==='third-progress');
  assert.equal(live[commentaryIndex].message.stop_reason,null,'commentary must not signal the end of a turn');
  assert.equal(live[commentaryIndex+1]?.event?.type,'message_start','working must remain visible after commentary');
  const resultIndex=live.findIndex(p=>p.message?.content?.some?.(b=>b.tool_use_id==='beeper-search'));
  assert.equal(live[resultIndex+1]?.event?.type,'message_start','working must resume between tools');
  const users = live.filter(p => p.type === 'user');
  assert.deepEqual(users.filter(p=>!p.message.content?.some?.(b=>b.type==='tool_result')).map(p => p.message.content), ['third', [{type:'text',text:'Native prompt'}]],
    'native prompts are mirrored once; bridge submissions are not duplicated');
  const approval = live.find(p => p.type === 'control_request' && p.request.tool_use_id === 'approval-third');
  assert.ok(live.some(p => p.type === 'control_cancel_request' && p.request_id === approval.request_id),
    'resolving approval in the native TUI cancels the remote prompt');
  for (const m of (await messages()).filter(m => ['turn/start','turn/steer'].includes(m.rpc?.method)))
    assert.match(m.rpc.params.clientUserMessageId, /^[0-9a-f-]{36}$/i);
  await stop();
  const before = (await messages()).length;
  output = ''; start();
  await waitFor(async () => (await messages()).slice(before).some(m => m.url?.includes('from_sequence_num=12')));
  const after = (await messages()).slice(before);
  assert.ok(after.some(m => m.rpc?.method === 'thread/resume'));
  assert.equal(JSON.parse(await readFile(state, 'utf8')).display_name, '[Codex] project · elephant');
  assert.ok(after.some(m => m.url?.includes('from_sequence_num=12')));
  assert.equal(after.some(m => ['turn/start', 'turn/steer'].includes(m.rpc?.method)), false, 'replayed prompts must not execute again');
  assert.equal(after.some(m => m.rpc?.method === 'thread/turns/list'), false, 'the same remote session already has its history');
  await stop();

  const failedStart = (await messages()).length;
  output = ''; start('cse_history', true);
  await waitFor(() => child.exitCode !== null);
  assert.equal(child.exitCode, 1);
  assert.equal(JSON.parse(await readFile(state, 'utf8')).remote_sid, 'cse_check', 'failed replay must not mark history uploaded');
  const failedHistory = (await messages()).slice(failedStart).flatMap(m => m.body?.events || []).map(e => e.payload);

  const historyStart = (await messages()).length;
  output = ''; start('cse_history');
  await waitFor(() => output.includes(' ready'));
  const replay = (await messages()).slice(historyStart);
  const history = replay.flatMap(m => m.body?.events || []).map(e => e.payload).filter(p => ['user','assistant'].includes(p.type));
  assert.deepEqual(history.map(p => [p.type,p.message.content]), [
    ['user', [{type:'text',text:'Earlier question'},{type:'text',text:'More context'}]],
    ['assistant', [{type:'text',text:'Earlier progress'}]],
    ['assistant', [{type:'tool_use',id:'old-command',name:'Bash',input:{command:'echo history',cwd:'/work'}}]],
    ['user', [{type:'tool_result',tool_use_id:'old-command',content:'history output',is_error:false}]],
    ['assistant', [{type:'text',text:'Earlier final answer'}]],
  ], 'all pages of conversation text must reach the new remote in order');
  for (const prior of failedHistory.filter(p => ['user','assistant'].includes(p.type))) {
    assert.equal(history.find(p => p.message.id === prior.message.id)?.uuid, prior.uuid, 'a retry must reuse UUIDs for remote deduplication');
  }
  assert.deepEqual(replay.filter(m => m.rpc?.method === 'thread/turns/list').map(m => m.rpc.params), [
    {threadId:'01234567-1234-1234-1234-0123456789ab',limit:50,sortDirection:'asc',itemsView:'full'},
    {threadId:'01234567-1234-1234-1234-0123456789ab',limit:50,sortDirection:'asc',itemsView:'full',cursor:'next-turn'},
  ]);
  await waitFor(async () => JSON.parse(await readFile(state, 'utf8')).remote_sequence === 2);
  assert.equal((await messages()).slice(historyStart).some(m => ['turn/start','turn/steer'].includes(m.rpc?.method)), false, 'uploaded user history must never become a new prompt');
  await stop();
  const historyRestart = (await messages()).length;
  output = ''; start('cse_history');
  await waitFor(() => output.includes(' ready'));
  assert.equal((await messages()).slice(historyRestart).some(m => m.rpc?.method === 'thread/turns/list'), false, 'worker restart must not duplicate history');
  await stop();
  const nextRemoteStart = (await messages()).length;
  output = ''; start('cse_next');
  await waitFor(() => output.includes(' ready'));
  const nextHistory = (await messages()).slice(nextRemoteStart).flatMap(m => m.body?.events || []).map(e => e.payload).filter(p => ['user','assistant'].includes(p.type));
  assert.equal(nextHistory.length, history.length, 'each new remote session needs the complete history');
  for (const p of nextHistory) assert.notEqual(p.uuid, history.find(h => h.message.id === p.message.id)?.uuid, 'separate remote sessions need distinct event UUIDs');
  await stop();
  const nativeStart = (await messages()).length;
  const nativeState = JSON.parse(await readFile(state,'utf8'));
  await writeFile(state,JSON.stringify({...nativeState,native_terminal:true,remote_user_ids:['own-remote-uuid']}));
  output = ''; start('cse_next', false, true);
  await waitFor(()=>output.includes(' ready'));
  assert.equal(JSON.parse(await readFile(state,'utf8')).working,true,'a reconnect must preserve the active native turn');
  const nativeSocket = createConnection(join(tmpdir(), `cdash-codex-${process.getuid()}-${createHash('sha256').update(state).digest('hex').slice(0,16)}`, 'input.sock'));
  await new Promise((r,j)=>{nativeSocket.once('error',j);nativeSocket.once('connect',()=>nativeSocket.end('third',r));});
  await waitFor(async()=> (await messages()).slice(nativeStart).some(m=>['turn/start','turn/steer'].includes(m.rpc?.method)));
  await waitFor(async()=> (await messages()).slice(nativeStart).some(m=>m.body?.events?.some(e=>e.payload.type==='result')));
  const nativeMessages=(await messages()).slice(nativeStart);
  assert.equal(nativeMessages.find(m=>m.body?.worker_status)?.body.worker_status,'running','RC reconnect must show the live turn');
  assert.ok(nativeMessages.some(m=>m.body?.events?.some(e=>e.payload.type==='stream_event' && e.payload.event.type==='message_start')),
    'reconnecting during a native turn must restore activity');
  const resume=nativeMessages.find(m=>m.rpc?.method==='thread/resume').rpc.params;
  assert.equal(resume.model,undefined,'worker reconnect must preserve native model choices');
  assert.equal(resume.config,undefined,'worker reconnect must preserve native effort choices');
  const caughtUp=nativeMessages.flatMap(m=>m.body?.events||[]).map(e=>e.payload);
  assert.ok(caughtUp.some(p=>p.message?.id==='offline-user'),'same-session reconnect must catch up native messages');
  assert.ok(caughtUp.some(p=>p.message?.id==='offline-reply'));
  assert.equal(caughtUp.some(p=>p.message?.id==='own-user'),false,'catch-up must not duplicate original RC user bubbles');
  const nativeSubmissions=nativeMessages.filter(m=>['turn/start','turn/steer'].includes(m.rpc?.method));
  assert.deepEqual(nativeSubmissions.map(m=>m.rpc.method),['turn/steer'],'an active native turn must not be duplicated');
  assert.equal(nativeSubmissions[0].rpc.params.expectedTurnId,'native-active');
  assert.equal(nativeMessages.some(m=>m.rpc?.id===500 && m.rpc.error),false,
    'requests the native TUI can answer must not be rejected by the bridge');
  for(const race of ['replay','init']){
    await stop();
    const raceStart=(await messages()).length;
    output='';start('cse_race_'+race,false,true,race);
    await waitFor(()=>output.includes(' ready'));
    await waitFor(async()=>(await messages()).slice(raceStart).some(m=>m.body?.events?.some(e=>e.payload.type==='result')));
    const raceEvents=(await messages()).slice(raceStart).flatMap(m=>m.body?.events||[]).map(e=>e.payload);
    const started=raceEvents.findIndex(p=>p.event?.type==='message_start');
    const stopped=raceEvents.findIndex(p=>p.event?.type==='message_stop');
    assert.ok(started>=0 && started<stopped,`a turn finishing during ${race} must signal activity before stopping`);
  }
});


test('Codex metadata handles authentication and bounded slow queries', { timeout: 30_000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-codex-auth-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  await writeFile(join(dir, 'codex'), `#!${process.execPath}
const {createInterface}=require('node:readline');
const delays=JSON.parse(process.env.CDASH_CHECK_DELAYS||'{}');
const send=(m,method)=>setTimeout(()=>process.stdout.write(JSON.stringify(m)+'\\n'),delays[method]||0);
createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);
 if(m.method==='initialize')process.stdout.write(JSON.stringify({id:m.id,result:{}})+'\\n');
 if(m.method==='account/read')send({id:m.id,result:JSON.parse(process.env.CDASH_CHECK_ACCOUNT)},m.method);
 if(m.method==='model/list')process.stdout.write(JSON.stringify({id:m.id,result:{data:[{model:'gpt-check',displayName:'GPT Check',supportedReasoningEfforts:[{reasoningEffort:'low'}],defaultReasoningEffort:'low'}],nextCursor:null}})+'\\n');
 if(m.method==='account/rateLimits/read')send({id:m.id,result:{rateLimits:{limitId:'codex',primary:{usedPercent:25,windowDurationMins:300,resetsAt:1730947200},secondary:{usedPercent:40,windowDurationMins:10080,resetsAt:1730950800}}}},m.method);
});
`, { mode: 0o700 });
  const discover = (account, mode = '--models', delays = {}) => new Promise((done, reject) => {
    const source = readFileSync(resolve('crates/agent/src/collect/codex-remote.mjs'), 'utf8');
    const child = spawn(process.execPath, ['--input-type=module', '-e', source, '--', mode], {
      cwd: dir,
      env: { ...process.env, PATH: dir, CDASH_CHECK_ACCOUNT: JSON.stringify(account), CDASH_CHECK_DELAYS: JSON.stringify(delays) },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', b => { stdout += b; });
    child.stderr.on('data', b => { stderr += b; });
    child.on('error', reject);
    child.on('exit', code => {
      if (code) reject(Error(stderr));
      else { try { done(JSON.parse(stdout)); } catch (e) { reject(e); } }
    });
  });
  assert.deepEqual(await discover({ account: null, requiresOpenaiAuth: true }), []);
  for (const account of [
    { account: { type: 'apiKey' }, requiresOpenaiAuth: true },
    { account: { type: 'chatgpt' }, requiresOpenaiAuth: true },
    { account: null, requiresOpenaiAuth: false },
  ]) assert.equal((await discover(account))[0].label, 'GPT Check');
  for (const account of [
    { account: null, requiresOpenaiAuth: true },
    { account: { type: 'apiKey' }, requiresOpenaiAuth: true },
    { account: null, requiresOpenaiAuth: false },
  ]) assert.equal(await discover(account, '--usage'), null);
  const usage = await discover({ account: { type: 'chatgpt' }, requiresOpenaiAuth: true }, '--usage');
  assert.equal(usage.rateLimits.primary.usedPercent, 25);
  assert.equal(usage.rateLimits.secondary.usedPercent, 40);
  const slow = await discover({ account: { type: 'chatgpt' }, requiresOpenaiAuth: true }, '--usage', {
    'account/read': 2100, 'account/rateLimits/read': 2100,
  });
  assert.equal(slow.rateLimits.primary.usedPercent, 25, 'valid sequential RPCs can take more than four seconds');
  const slowAuth = await discover({ account: { type: 'chatgpt' }, requiresOpenaiAuth: true }, '--usage', {
    'account/read': 3200,
  });
  assert.equal(slowAuth.rateLimits.secondary.usedPercent, 40, 'account reads can take more than three seconds');
  const timeout = await discover({ account: { type: 'chatgpt' }, requiresOpenaiAuth: true }, '--usage', {
    'account/read': 11000,
  });
  assert.deepEqual(timeout, { error: 'Codex account/read timed out' });
});

test('Codex --threads lists only unarchived dashboard threads from the newest state database', { timeout: 15_000 }, async t => {
  const { DatabaseSync } = await import('node:sqlite');
  const home = await mkdtemp(join(tmpdir(), 'cdash-codex-threads-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const make = (file, rows) => {
    const db = new DatabaseSync(join(home, file));
    db.exec(`create table threads (id text primary key, cwd text, title text, name text,
      updated_at_ms integer, model text, reasoning_effort text, originator text, archived integer,
      rollout_path text default '/work/history.jsonl')`);
    const add = db.prepare('insert into threads (id,cwd,title,name,updated_at_ms,model,reasoning_effort,originator,archived) values (?,?,?,?,?,?,?,?,?)');
    for (const r of rows) add.run(...r);
    db.close();
  };
  // An older schema generation must lose to the newer one.
  make('state_4.sqlite', [['stale', '/old', 'stale', null, 1, 'old', 'low', 'claude_dashboard', 0]]);
  make('state_5.sqlite', [
    ['a', '/work/a', 'first prompt', null, 300, 'gpt-b', 'high', 'claude_dashboard', 0],
    ['b', '/work/b', 'old title', 'Renamed', 200, null, null, 'claude_dashboard', 0],
    ['c', '/work/c', 'archived', null, 100, 'gpt-b', 'low', 'claude_dashboard', 1],
    ['d', '/work/d', 'from the codex cli', null, 400, 'gpt-b', 'low', 'codex-tui', 0],
    ['e', '/work/e', '', null, 500, 'gpt-b', 'low', 'claude_dashboard', 0],
  ]);
  const run = codexHome => new Promise((done, reject) => {
    // Exactly how the agent runs it (`codex::query`): as `-e` source, where the
    // flag is process.argv[1], not part of a script path's arguments.
    const source = readFileSync(resolve('crates/agent/src/collect/codex-remote.mjs'), 'utf8');
    const child = spawn(process.execPath, ['--input-type=module', '-e', source, '--', '--threads'], {
      env: { ...process.env, CODEX_HOME: codexHome }, stdio: ['ignore', 'pipe', 'ignore'],
    });
    let out = '';
    child.stdout.on('data', b => { out += b; });
    child.on('error', reject);
    child.on('exit', () => { try { done(JSON.parse(out)); } catch (e) { reject(e); } });
  });
  assert.deepEqual(await run(home), [
    { sid: 'a', dir: '/work/a', ts: 300, model: 'gpt-b', effort: 'high', rollout_path: '/work/history.jsonl' },
    { sid: 'b', dir: '/work/b', ts: 200, model: null, effort: null, rollout_path: '/work/history.jsonl' },
  ]);
  assert.deepEqual(await run(join(home, 'no-codex-here')), [], 'no Codex state is an empty list, not an error');
});

test('copied Codex worker reports missing Claude transport before connecting', { timeout: 10_000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-worker-package-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const bridge = join(dir, 'bridge.mjs'), state = join(dir, 'state.json');
  await writeFile(bridge, await readFile(resolve('crates/agent/src/collect/codex-remote.mjs')));
  await writeFile(state, '{}');
  const result = await new Promise((done, reject) => {
    const child = spawn(process.execPath, [bridge, '--worker', '--sdk-url', 'http://invalid', '--session-id', 'fixture'], {
      env: { ...process.env, CDASH_CODEX_STATE: state, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'fixture', CLAUDE_CODE_WORKER_EPOCH: '1' },
      stdio: ['pipe', 'ignore', 'pipe'],
    });
    let stderr = '';
    child.stderr.on('data', data => stderr += data);
    child.on('error', reject);
    child.on('exit', code => done({ code, stderr }));
    child.stdin.end();
  });
  assert.notEqual(result.code, 0);
  assert.match(result.stderr, /claude-remote\.mjs/);
});

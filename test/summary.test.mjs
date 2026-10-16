import test from 'node:test';
import assert from 'node:assert/strict';
import { conversation, summaryInput, validSummary, validateOutput } from '../crates/agent/src/collect/summary.mjs';

const native = 'The user wanted to fix dashboard session resuming while preserving previous model choices and avoiding unnecessary work after restarting the application. The implementation added saved settings and checked the parser against historical sessions before updating the visible resumable session cards for review. Local tests passed for the changed components while device checks and real provider authentication remained unverified and needed separate validation before release. The agreed next step was to review the patch and open a pull request while keeping deployment pending until the user approved it.';
const line = x => JSON.stringify(x);
const user = text => ({ type: 'user', message: { content: text } });
const assistant = text => ({ type: 'assistant', message: { content: [{ type: 'thinking', thinking: 'private' }, { type: 'text', text }] } });

test('extracts conversation text without thinking, tools or metadata; hashes ignore metadata', () => {
  const rows = [user('Fix resuming'), assistant('Changed parser; tests still pending.'), { type: 'progress', data: 'noise' }, { type: 'user', message: { content: [{ type: 'tool_result', content: 'huge' }] } }];
  const a = conversation(rows.map(line).join('\n'));
  const b = conversation(rows.slice(0, 2).map(x => line({ ...x, timestamp: 99 })).join('\n'));
  assert.deepEqual(a, b);
  assert.equal(a.messages.length, 2);
  assert.doesNotMatch(JSON.stringify(a), /private|huge|noise/);
});

test('reads Codex response items once, readable compaction, and ignores encrypted data', () => {
  const a = conversation([
    { type: 'response_item', payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'Fix resuming' }] } },
    { type: 'event_msg', payload: { type: 'user_message', message: 'Fix resuming' } },
    { type: 'compacted', payload: { message: native, encrypted_content: 'unreadable' } },
  ].map(line).join('\n'));
  assert.equal(a.native, native);
  assert.equal(a.messages.length, 2);
  assert.doesNotMatch(JSON.stringify(a), /unreadable/);
  assert.equal(conversation(line({ type: 'compacted', payload: { encrypted_content: 'x' } })).native, null);
});

test('reuses only a current native summary; newer turns must update it', () => {
  const compact = { type: 'user', isCompactSummary: true, message: { content: native } };
  assert.equal(conversation(line(compact)).native, native);
  assert.equal(conversation([compact, user('Actually keep deployment pending')].map(line).join('\n')).native, null);
  assert.ok(validSummary(native));
  assert.equal(validSummary('Short label'), false);
});

test('append-only changes use prior summary plus new messages; rewrites use full context', () => {
  const old = conversation([user('Goal'), assistant('Tests pending')].map(line).join('\n'));
  const cache = { summary: native, count: old.messages.length, prefix: old.revision };
  const next = conversation([user('Goal'), assistant('Tests pending'), user('Tests now pass')].map(line).join('\n'));
  const input = summaryInput(next, cache);
  assert.match(input, /Previous summary/);
  assert.match(input, /Tests now pass/);
  assert.doesNotMatch(input, /assistant: Tests pending/);
  const rewritten = conversation([user('Different goal'), assistant('Tests pending'), user('Tests now pass')].map(line).join('\n'));
  assert.doesNotMatch(summaryInput(rewritten, cache), /Previous summary/);
});

test('structured output rejects missing fields and joined words', () => {
  const sentences = native.match(/[^.!?]+[.!?]/g).map(s => s.trim());
  const fields = Object.fromEntries(['goal', 'work', 'status', 'next'].map((k, i) => [k, sentences[i]]));
  assert.equal(validateOutput(fields), native);
  assert.throws(() => validateOutput({ ...fields, next: 'No further actions.' }));
  assert.throws(() => validateOutput({ ...fields, extra: 'unexpected' }));
});

import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, readFile, rm, stat, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-summary-test-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const wire = join(dir, 'wire.jsonl');
  await writeFile(wire, '');
  const output = Object.fromEntries(['goal', 'work', 'status', 'next'].map((k, i) => [k, native.match(/[^.!?]+[.!?]/g)[i].trim()]));
  await writeFile(join(dir, 'codex'), `#!${process.execPath}
const {createInterface} = require('node:readline');
const {appendFileSync,readFileSync,writeFileSync,existsSync} = require('node:fs');
const send = m => process.stdout.write(JSON.stringify(m)+'\\n');
createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);
 appendFileSync(process.env.CDASH_CHECK_WIRE, JSON.stringify({kind:'codex',rpc:m,home:process.env.CODEX_HOME,secret:process.env.OPENAI_API_KEY,config:readFileSync(process.env.CODEX_HOME+'/config.toml','utf8'),catalog:existsSync(process.env.CODEX_HOME+'/summary-model.json')?JSON.parse(readFileSync(process.env.CODEX_HOME+'/summary-model.json','utf8')):null})+'\\n');
 if(m.method==='initialize')send({id:m.id,result:{}});
 if(m.method==='account/read')send({id:m.id,result:{account:{type:process.env.CDASH_CHECK_AUTH||'chatgpt'}}});
 if(m.method==='account/rateLimits/read')send({id:m.id,result:{rateLimits:{primary:{usedPercent:Number(process.env.CDASH_CHECK_QUOTA||1)},secondary:{usedPercent:2}}}});
 if(m.method==='model/list'){
  writeFileSync(process.env.CODEX_HOME+'/models_cache.json',JSON.stringify({models:[{slug:'gpt-6-luna',tool_mode:'code_mode_only',experimental_supported_tools:['clock','send_user_message_async'],supports_search_tool:true}]}));
  send({id:m.id,result:{data:[{model:'gpt-6-luna',supportedReasoningEfforts:[{reasoningEffort:'low'}]}]}});
 }
 if(m.method==='thread/start')send({id:m.id,result:{thread:{id:'test-thread'}}});
 if(m.method==='turn/start'){
   send({id:m.id,result:{turn:{id:'turn'}}});
   if(process.env.CDASH_CHECK_TOOL)send({id:99,method:'item/commandExecution/requestApproval',params:{}});
   send({method:'item/completed',params:{item:{type:'agentMessage',text:JSON.stringify(JSON.parse(process.env.CDASH_CHECK_OUTPUT))}}});
   send({method:'turn/completed',params:{turn:{status:'completed'}}});
 }
});
`, { mode: 0o700 });
  await writeFile(join(dir, 'claude'), `#!${process.execPath}
const {appendFileSync}=require('node:fs');
appendFileSync(process.env.CDASH_CHECK_WIRE,JSON.stringify({kind:'claude',args:process.argv.slice(2),secret:process.env.ANTHROPIC_API_KEY})+'\\n');
if(process.argv[2]==='auth')console.log(JSON.stringify({loggedIn:true,authMethod:'claude.ai'}));
else {
 let input='';process.stdin.on('data',b=>input+=b);process.stdin.on('end',()=>{
  appendFileSync(process.env.CDASH_CHECK_WIRE,JSON.stringify({kind:'input',input})+'\\n');
  console.log(JSON.stringify({structured_output:JSON.parse(process.env.CDASH_CHECK_OUTPUT),modelUsage:{'claude-haiku-5-5':{}},is_error:false}));
 });
}
`, { mode: 0o700 });
  const source = join(dir, 'session.jsonl'), cache = join(dir, 'summary.json');
  await writeFile(source, [user('Fix resumable sessions'), assistant('Local parser checks passed; device tests pending.')].map(line).join('\n'));
  const run = (extra = {}) => new Promise((done, reject) => {
    const child = spawn(process.execPath, [resolve('crates/agent/src/collect/codex-remote.mjs'), '--summary', JSON.stringify({ source, cache })], {
      env: { ...process.env, PATH: dir, CDASH_CHECK_WIRE: wire, CDASH_CHECK_OUTPUT: JSON.stringify(output),
        OPENAI_API_KEY: 'must-not-leak', ANTHROPIC_API_KEY: 'must-not-leak', ...extra }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', b => stdout += b);child.stderr.on('data', b => stderr += b);
    child.on('error', reject);
    child.on('exit', code => code ? reject(Error(stderr)) : done(JSON.parse(stdout)));
  });
  const events = async () => (await readFile(wire, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
  return { dir, wire, source, cache, run, events };
}

test('Luna uses meter RPCs and isolated Low; restart/metadata changes make no inference calls', { timeout: 15_000 }, async t => {
  const f = await fixture(t);
  assert.equal((await f.run()).summary, native);
  let events = await f.events();
  assert.ok(events.some(e => e.rpc?.method === 'account/rateLimits/read'));
  const thread = events.find(e => e.rpc?.method === 'thread/start').rpc.params;
  const turn = events.find(e => e.rpc?.method === 'turn/start').rpc.params;
  assert.equal(thread.ephemeral, true);
  assert.equal(thread.allowProviderModelFallback, false);
  assert.deepEqual(thread.dynamicTools, []);
  assert.deepEqual(thread.environments, []);
  assert.deepEqual(thread.selectedCapabilityRoots, []);
  assert.match(events[0].config, /skip_host_skill_discovery=true/);
  assert.match(events[0].config, /include_instructions=false/);
  assert.match(events[0].config, /image_generation=false/);
  const configured = events.find(e => e.rpc?.method === 'thread/start');
  assert.match(configured.config, /model_catalog_json=/);
  assert.equal(configured.catalog.models[0].tool_mode, 'direct');
  assert.deepEqual(configured.catalog.models[0].experimental_supported_tools, []);
  assert.match(thread.baseInstructions, /only agreed unfinished actions/);
  assert.equal(turn.model, 'gpt-6-luna');assert.equal(turn.effort, 'low');
  assert.ok(events.every(e => !e.secret));
  assert.equal((await stat(f.cache)).mode & 0o777, 0o600);
  await f.run({ CDASH_CHECK_AUTH: 'apiKey' });
  await writeFile(f.source, (await readFile(f.source, 'utf8')) + '\n' + line({ type: 'progress', timestamp: 99 }));
  await f.run();
  assert.equal((await f.events()).filter(e => e.rpc?.method === 'turn/start').length, 1);
  assert.equal((await f.events()).filter(e => e.kind === 'claude').length, 0);
});

test('quota/auth/tool failures fall back to Haiku Low with identical prompt and schema', { timeout: 20_000 }, async t => {
  const f = await fixture(t);
  for (const setting of [{ CDASH_CHECK_QUOTA: '100' }, { CDASH_CHECK_AUTH: 'apiKey' }, { CDASH_CHECK_TOOL: 'yes' }]) {
    await rm(f.cache, { force: true });
    assert.equal((await f.run(setting)).summary, native);
  }
  const events = await f.events();
  const haiku = events.filter(e => e.kind === 'claude' && e.args.includes('-p'));
  assert.equal(haiku.length, 3);
  const luna = events.find(e => e.rpc?.method === 'thread/start').rpc.params;
  const turn = events.find(e => e.rpc?.method === 'turn/start').rpc.params;
  for (const h of haiku) {
    const value = flag => h.args[h.args.indexOf(flag) + 1];
    assert.equal(value('--system-prompt'), luna.baseInstructions);
    assert.deepEqual(JSON.parse(value('--json-schema')), turn.outputSchema);
    assert.equal(value('--model'), 'claude-haiku-5-5');assert.equal(value('--effort'), 'low');
    assert.equal(value('--tools'), '');assert.ok(h.args.includes('--safe-mode'));
    assert.ok(h.args.includes('--no-session-persistence'));assert.ok(!h.secret);
  }
  assert.equal(events.filter(e => e.rpc?.method === 'turn/start').length, 1, 'only the tool-request case starts a Luna turn and still falls back');
});

test('native summaries bypass CLIs and new turns update incrementally; failed refresh keeps disk cache', { timeout: 15_000 }, async t => {
  const f = await fixture(t);
  await writeFile(f.source, line({ type: 'user', isCompactSummary: true, message: { content: native } }));
  assert.equal((await f.run()).summary, native);
  assert.equal((await f.events()).length, 0);
  await writeFile(f.source, (await readFile(f.source, 'utf8')) + '\n' + line(user('Deployment now approved, still unrun.')));
  await f.run({ CDASH_CHECK_AUTH: 'apiKey' });
  assert.match((await f.events()).find(e => e.kind === 'input').input, /Previous summary[\s\S]*Deployment now approved/);
  const before = await readFile(f.cache, 'utf8');
  await writeFile(f.source, (await readFile(f.source, 'utf8')) + '\n' + line(user('One more request')));
  assert.ok((await f.run({ CDASH_CHECK_OUTPUT: '{}' })).error);
  assert.equal(await readFile(f.cache, 'utf8'), before);
});

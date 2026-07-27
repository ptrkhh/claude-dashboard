import test from 'node:test';
import assert from 'node:assert/strict';
import { usablePrompts, groupHistory, parseTranscript, parseRcFile, parseTmuxPanes, parseGitStatus, projectDirName } from '../lib/sessions.js';

test('usablePrompts filters junk', () => {
  assert.deepEqual(
    usablePrompts(['/model', '', 'ok', 'YES', 'fix the auth bug', 'continue', 'add tests']),
    ['fix the auth bug', 'add tests']
  );
});

test('groupHistory groups, sorts newest-first, keeps last 3 usable prompts', () => {
  const lines = [
    JSON.stringify({ sessionId: 'a', project: '/x', timestamp: 100, display: 'first prompt' }),
    JSON.stringify({ sessionId: 'b', project: '/y', timestamp: 300, display: 'other session' }),
    JSON.stringify({ sessionId: 'a', project: '/x', timestamp: 200, display: 'p2' }),
    JSON.stringify({ sessionId: 'a', project: '/x', timestamp: 250, display: 'p3' }),
    JSON.stringify({ sessionId: 'a', project: '/x', timestamp: 260, display: 'p4' }),
    'not json — must be skipped',
  ].join('\n');
  const g = groupHistory(lines);
  assert.equal(g[0].sid, 'b');
  assert.equal(g[1].sid, 'a');
  assert.equal(g[1].ts, 260);
  assert.equal(g[1].cwd, '/x');
  assert.deepEqual(g[1].prompts, ['p2', 'p3', 'p4']);
});

test('parseTranscript extracts branch, title, counts, last assistant text', () => {
  const lines = [
    JSON.stringify({ type: 'user', gitBranch: 'main', message: {} }),
    JSON.stringify({ type: 'ai-title', aiTitle: 'Fix auth bug' }),
    JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: 'first reply' }] } }),
    JSON.stringify({ type: 'assistant', message: { content: [{ type: 'tool_use' }, { type: 'text', text: 'done, tests pass' }] } }),
  ].join('\n');
  const t = parseTranscript(lines);
  assert.equal(t.branch, 'main');
  assert.equal(t.title, 'Fix auth bug');
  assert.equal(t.assistantCount, 2);
  assert.equal(t.lastAssistantText, 'done, tests pass');
});

test('parseTranscript drops HEAD branch, nulls when absent', () => {
  const t = parseTranscript(JSON.stringify({ type: 'user', gitBranch: 'HEAD' }));
  assert.equal(t.branch, null);
  assert.equal(t.title, null);
  assert.equal(t.lastAssistantText, null);
});

test('parseRcFile', () => {
  assert.equal(parseRcFile('{"bridgeSessionId":"session_abc123"}'), 'session_abc123');
  assert.equal(parseRcFile('garbage'), null);
});

test('parseTmuxPanes filters to cdash-', () => {
  const out = 'cdash-backend-1531|4242|/mnt/d/git/backend|1785050000\nother|1|/tmp|1785050001\n';
  assert.deepEqual(parseTmuxPanes(out), [
    { name: 'cdash-backend-1531', pid: 4242, path: '/mnt/d/git/backend', created: 1785050000 },
  ]);
});

test('parseGitStatus', () => {
  const out = '## main...origin/main [ahead 2, behind 1]\n M server.js\n?? new.txt\n';
  assert.deepEqual(parseGitStatus(out), { branch: 'main', dirty: 2, ahead: 2, behind: 1 });
  assert.deepEqual(parseGitStatus('## feature-x\n'), { branch: 'feature-x', dirty: 0, ahead: 0, behind: 0 });
});

test('projectDirName munges path', () => {
  assert.equal(projectDirName('/mnt/d/git/backend'), '-mnt-d-git-backend');
});

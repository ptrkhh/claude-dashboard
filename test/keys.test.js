import test from 'node:test';
import assert from 'node:assert/strict';
import { parseSendKeys, sendKeysArgs } from '../lib/keys.js';

const throws400 = (body, re) =>
  assert.throws(() => parseSendKeys(body), e => e.status === 400 && re.test(e.message));

test('accepts a cdash session name and trims the text', () => {
  assert.deepEqual(
    parseSendKeys({ name: 'cdash-infra-0847-k2p', text: '  ! gcloud auth login  ' }),
    { name: 'cdash-infra-0847-k2p', text: '! gcloud auth login' },
  );
});

test('rejects names outside the cdash namespace', () => {
  for (const name of ['other-session', 'cdash-a; rm -rf /', 'cdash-a b', '', undefined, 42])
    throws400({ name, text: 'hi' }, /bad name/);
});

test('rejects missing, empty, and whitespace-only text', () => {
  throws400({ name: 'cdash-a' }, /text required/);
  throws400({ name: 'cdash-a', text: 42 }, /text required/);
  throws400({ name: 'cdash-a', text: '   ' }, /empty text/);
  throws400({ name: 'cdash-a', text: '\n\n' }, /empty text/);
});

test('rejects text past the length cap', () => {
  throws400({ name: 'cdash-a', text: 'x'.repeat(4097) }, /too long/);
});

test('collapses newlines so a paste submits once', () => {
  // A multi-line paste would otherwise fire line 1 and leave the rest to land
  // in whatever prompt came next.
  assert.equal(parseSendKeys({ name: 'cdash-a', text: 'one\ntwo\r\nthree' }).text, 'one two three');
});

test('strips control characters that would act as keys in the TUI', () => {
  assert.equal(parseSendKeys({ name: 'cdash-a', text: 'safe\x1b[Atext\x00' }).text, 'safe[Atext');
});

test('sendKeysArgs passes text as a literal operand after --', () => {
  // Without --, tmux reads a leading-dash command as its own flag and errors.
  const [literal, enter] = sendKeysArgs({ name: 'cdash-a', text: '--version' });
  assert.deepEqual(literal, ['send-keys', '-t', 'cdash-a', '-l', '--', '--version']);
  assert.deepEqual(enter, ['send-keys', '-t', 'cdash-a', 'Enter']);
});

import { test } from 'node:test';
import assert from 'node:assert/strict';
import '../public/browser/view.js';
const B = globalThis.cdashBrowser;

test('taps map from displayed image pixels to page CSS pixels', () => {
  const meta = { deviceWidth: 985, deviceHeight: 640 };
  assert.deepEqual(B.toPage(0, 0, { w: 394, h: 256 }, meta), [0, 0]);
  assert.deepEqual(B.toPage(197, 128, { w: 394, h: 256 }, meta), [492.5, 320]);
  assert.deepEqual(B.toPage(394, 256, { w: 788, h: 512 }, meta), [492.5, 320], '2x image zoom');
});

test('keyboard input becomes text or keys', () => {
  assert.deepEqual(B.fromInput('insertText', 'é'), [{ text: 'é' }]);
  assert.deepEqual(B.fromInput('insertFromPaste', '123456'), [{ text: '123456' }]);
  assert.deepEqual(B.fromInput('deleteContentBackward', null), [{ key: 'Backspace' }]);
  assert.deepEqual(B.fromInput('insertLineBreak', null), [{ key: 'Enter' }]);
  assert.deepEqual(B.fromInput('insertText', ''), []);
  assert.deepEqual(B.fromInput('formatBold', null), []);
  assert.deepEqual(B.fromInput('insertCompositionText', 'に'), [], 'partial composition is not committed input');
  assert.deepEqual(Object.values(B.KEYBAR).sort(), ['ArrowLeft', 'ArrowRight', 'Backspace', 'Enter', 'Escape', 'Tab']);
});

import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/md.js');
const { render } = globalThis.cdashMd;

test('nothing in the input can become markup', () => {
  const html = render('<img src=x onerror=alert(1)> <script>x</script> **<b>bold</b>**');
  assert.ok(!/<img|<script|<b>/i.test(html));
  assert.match(html, /&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.match(html, /<strong>&lt;b&gt;bold&lt;\/b&gt;<\/strong>/);
});
test('links keep only safe schemes and cannot break out of the attribute', () => {
  assert.match(render('[ok](https://example.com/a?b=1&c=2)'), /<a href="https:\/\/example\.com\/a\?b=1&amp;c=2" target="_blank" rel="noopener noreferrer">ok<\/a>/);
  for (const bad of ['javascript:alert(1)', 'JaVaScRiPt:alert(1)', 'data:text/html,x', 'vbscript:x', ' javascript:x'])
    assert.ok(!render(`[x](${bad})`).includes('<a '), bad);
  assert.ok(!render('[x](https://a.b/"onmouseover="alert(1))').includes('"onmouseover'));
});
test('the constructs Claude replies use are rendered', () => {
  assert.match(render('# Title'), /<h1>Title<\/h1>/);
  assert.match(render('a `code` b'), /a <code>code<\/code> b/);
  assert.match(render('- one\n- two'), /<ul><li>one<\/li><li>two<\/li><\/ul>/);
  assert.match(render('1. one\n2. two'), /<ol><li>one<\/li><li>two<\/li><\/ol>/);
  assert.match(render('```js\nconst a = "<b>";\n```'), /<pre><code class="lang-js">const a = &quot;&lt;b&gt;&quot;;\n<\/code><\/pre>/);
  assert.match(render('> quoted'), /<blockquote>/);
  assert.match(render('| a | b |\n|---|---|\n| 1 | 2 |'), /<table>.*<th>a<\/th>.*<td>2<\/td>.*<\/table>/s);
  assert.match(render('one\n\ntwo'), /<p>one<\/p><p>two<\/p>/);
});
test('half-written text renders without hanging or leaking', () => {
  assert.match(render('```py\nprint(1)'), /<pre><code class="lang-py">print\(1\)\n?<\/code><\/pre>/);
  assert.equal(typeof render('**unclosed [link](http://'), 'string');
  assert.equal(render(''), '');
  const started = Date.now();
  render('*'.repeat(20000) + '['.repeat(20000));
  assert.ok(Date.now() - started < 1000, 'no catastrophic backtracking');
});

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

// Exercise the shipped theme handler without starting the network poll loop.
const app = readFileSync(new URL('../public/app.js', import.meta.url), 'utf8');
const theme = app.slice(app.indexOf('const THEME_KEY ='), app.indexOf('/* ---------- Launcher controls'));

function page({ stored = null, systemDark = false, deniedStorage = false } = {}) {
  const nodes = {
    '#theme-toggle': { setAttribute() {} },
    '#favicon': { href: 'icon.svg', setAttribute(key, value) { this[key] = value; } },
    '#favicon-png': { href: 'icon-32.png', setAttribute(key, value) { this[key] = value; } },
  };
  const root = { dataset: {} };
  let onSystemChange;
  const media = { matches: systemDark, addEventListener(_, callback) { onSystemChange = callback; } };
  const context = {
    $: selector => nodes[selector],
    ICONS: { system: '', sun: '', moon: '' },
    document: {
      documentElement: root,
      querySelectorAll: () => [],
      createElement: () => ({}),
      head: { appendChild() {} },
    },
    matchMedia: () => media,
  };
  Object.defineProperty(context, 'localStorage', { get() {
    if (deniedStorage) throw new Error('DOM storage disabled');
    return {
      getItem: () => stored,
      setItem: (_, value) => { stored = value; },
      removeItem: () => { stored = null; },
    };
  } });
  runInNewContext(theme, context);
  return {
    root, nodes,
    click() { nodes['#theme-toggle'].onclick(); },
    system(dark) { media.matches = dark; onSystemChange(); },
  };
}

function expectAppearance(ui, mode, svg, png) {
  assert.equal(ui.root.dataset.theme, mode);
  assert.equal(ui.nodes['#favicon'].href, svg);
  assert.equal(ui.nodes['#favicon-png'].href, png);
}

test('favicon follows system changes while theme mode is system', () => {
  const ui = page();
  expectAppearance(ui, 'light', 'icon-light.svg', 'icon-light-32.png');
  ui.system(true);
  expectAppearance(ui, 'dark', 'icon.svg', 'icon-32.png');
  ui.system(false);
  expectAppearance(ui, 'light', 'icon-light.svg', 'icon-light-32.png');
});

test('saved light and dark overrides win over system changes', () => {
  for (const [stored, svg, png] of [
    ['light', 'icon-light.svg', 'icon-light-32.png'],
    ['dark', 'icon.svg', 'icon-32.png'],
  ]) {
    const ui = page({ stored, systemDark: stored !== 'dark' });
    expectAppearance(ui, stored, svg, png);
    ui.system(stored === 'dark');
    expectAppearance(ui, stored, svg, png);
  }
});

test('theme toggle updates favicon and resumes following the system', () => {
  const ui = page();
  ui.click();
  expectAppearance(ui, 'light', 'icon-light.svg', 'icon-light-32.png');
  ui.system(true);
  expectAppearance(ui, 'light', 'icon-light.svg', 'icon-light-32.png');
  ui.click();
  expectAppearance(ui, 'dark', 'icon.svg', 'icon-32.png');
  ui.click();
  ui.system(false);
  expectAppearance(ui, 'light', 'icon-light.svg', 'icon-light-32.png');
});

test('denied local storage still resolves the system theme and updates favicon', () => {
  const ui = page({ deniedStorage: true, systemDark: true });
  expectAppearance(ui, 'dark', 'icon.svg', 'icon-32.png');
  ui.system(false);
  expectAppearance(ui, 'light', 'icon-light.svg', 'icon-light-32.png');
  assert.doesNotThrow(() => ui.click());
});

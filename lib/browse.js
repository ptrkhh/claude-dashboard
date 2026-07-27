import fs from 'node:fs/promises';
import path from 'node:path';

// Directory browser for the folder picker. Folders only (you're choosing a
// project directory), sorted case-insensitively, dotfolders hidden unless
// asked for. Entries are capped so an enormous directory can't stall a click.
const MAX_ENTRIES = 1000;

export async function listDirs(target, { showHidden = false } = {}) {
  const abs = path.resolve(target || '/');
  const dirents = await fs.readdir(abs, { withFileTypes: true }); // throws → handled by caller

  let names = dirents
    // Real directories, plus symlinks (which commonly point at directories).
    .filter(d => d.isDirectory() || d.isSymbolicLink())
    .map(d => d.name)
    .filter(name => showHidden || !name.startsWith('.'));

  names.sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' }));

  const truncated = names.length > MAX_ENTRIES;
  if (truncated) names = names.slice(0, MAX_ENTRIES);

  return {
    path: abs,
    parent: abs === path.parse(abs).root ? null : path.dirname(abs),
    entries: names.map(name => ({ name, path: path.join(abs, name) })),
    truncated,
  };
}

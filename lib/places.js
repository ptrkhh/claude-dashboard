import fs from 'node:fs/promises';

// Server-side recents + favorites for the folder picker, persisted as a small
// JSON file. Recents are recorded automatically when a session is launched;
// favorites are toggled explicitly from the picker.

export const MAX_RECENTS = 12;

// Pure helpers (unit-tested) — the file-backed functions below just wrap these.
export function pushRecent(list, p, max = MAX_RECENTS) {
  return [p, ...list.filter(x => x !== p)].slice(0, max);
}
export function toggleIn(list, p) {
  return list.includes(p) ? list.filter(x => x !== p) : [...list, p];
}

const EMPTY = { recents: [], favorites: [] };

export async function readPlaces(file) {
  try {
    const j = JSON.parse(await fs.readFile(file, 'utf8'));
    return {
      recents: Array.isArray(j.recents) ? j.recents : [],
      favorites: Array.isArray(j.favorites) ? j.favorites : [],
    };
  } catch { return { ...EMPTY }; }
}

async function writePlaces(file, data) {
  const tmp = `${file}.tmp`;
  await fs.writeFile(tmp, JSON.stringify(data, null, 2));
  await fs.rename(tmp, file); // atomic replace, same as the trust-file write
}

export async function addRecent(file, p) {
  const data = await readPlaces(file);
  data.recents = pushRecent(data.recents, p);
  await writePlaces(file, data);
  return data;
}

export async function toggleFavorite(file, p) {
  const data = await readPlaces(file);
  data.favorites = toggleIn(data.favorites, p);
  await writePlaces(file, data);
  return data;
}

import fs from 'node:fs/promises';
import path from 'node:path';

// Claude subscription usage limits — the same numbers `claude /usage` shows.
// They come from the OAuth-only endpoint GET /api/oauth/usage, authenticated
// with the subscription token Claude Code stores in ~/.claude/.credentials.json.
// Response shape: { five_hour: {utilization, resets_at}, seven_day: {...},
// seven_day_<model>: {...}, ... } where utilization is a 0–100 percentage.
const OAUTH_BETA = 'oauth-2025-04-20';
const BASE = (process.env.ANTHROPIC_BASE_URL || 'https://api.anthropic.com').replace(/\/+$/, '');

const cap = s => s.charAt(0).toUpperCase() + s.slice(1);

// short: the stat-tile label (shown as "Claude <short>"); long: the tooltip.
function labelsFor(key) {
  if (key === 'five_hour') return { short: 'Session', long: 'Current session' };
  if (key === 'seven_day') return { short: 'Week', long: 'Current week (all models)' };
  const wk = /^seven_day_(.+)$/.exec(key);
  if (wk) return { short: cap(wk[1]), long: `Current week (${cap(wk[1])})` };
  const sh = /^five_hour_(.+)$/.exec(key);
  if (sh) return { short: `Session ${cap(sh[1])}`, long: `Current session (${cap(sh[1])})` };
  return { short: key, long: key };
}

// session first, weekly-all-models next, model-specific weeklies after.
const ORDER = ['five_hour', 'seven_day'];
const rank = key => { const i = ORDER.indexOf(key); return i === -1 ? ORDER.length : i; };

// Normalize the raw /api/oauth/usage body into an ordered list of limit tiles.
// Ignores anything that isn't a { utilization: number } bucket, so unknown
// future fields (metadata, new bucket types) never break the strip.
export function parseUsage(data) {
  if (!data || typeof data !== 'object') return [];
  return Object.entries(data)
    .filter(([, v]) => v && typeof v === 'object' && typeof v.utilization === 'number')
    .map(([key, v]) => {
      const { short, long } = labelsFor(key);
      return {
        key, short, long,
        pct: Math.max(0, Math.min(100, Math.round(v.utilization))),
        resetsAt: v.resets_at || null,
      };
    })
    .sort((a, b) => rank(a.key) - rank(b.key) || a.key.localeCompare(b.key));
}

// The subscription token, or null for API-key users / logged-out / expired
// tokens (in which case we simply show no Claude tiles rather than 401ing).
async function oauthToken(claudeDir) {
  try {
    const txt = await fs.readFile(path.join(claudeDir, '.credentials.json'), 'utf8');
    const oauth = JSON.parse(txt).claudeAiOauth;
    if (!oauth?.accessToken) return null;
    if (oauth.expiresAt && Date.now() > oauth.expiresAt) return null; // stale — let the CLI refresh it
    return oauth.accessToken;
  } catch { return null; }
}

// Fetch + parse the live limits, or null if unavailable (no token, network
// error, non-2xx). Time-boxed so it can never stall a caller.
export async function fetchUsage(claudeDir) {
  const token = await oauthToken(claudeDir);
  if (!token) return null;
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 5000);
  try {
    const res = await fetch(`${BASE}/api/oauth/usage`, {
      headers: {
        Authorization: `Bearer ${token}`,
        'anthropic-beta': OAUTH_BETA,
        'Content-Type': 'application/json',
      },
      signal: ctl.signal,
    });
    if (!res.ok) return null;
    return parseUsage(await res.json());
  } catch { return null; }
  finally { clearTimeout(timer); }
}

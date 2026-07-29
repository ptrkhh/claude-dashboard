// Typing into a running session's TUI from the dashboard.
//
// The Claude app's remote control can send prompts but can't type into the TUI
// itself, so when a session stops and asks you to run something ("please run
// `! gcloud auth login`") there's no way to answer it from a phone. This sends
// the keystrokes straight to the tmux pane instead. Stopgap — delete the whole
// feature once remote control grows an input.

// Same guard /api/kill uses: only panes this dashboard named.
const NAME_RE = /^cdash-[\w-]+$/;
const MAX_TEXT = 4096;

// C0 controls minus the newlines parseSendKeys collapses, plus DEL. Stripped so a paste
// can't smuggle escape sequences (arrow keys, mode switches) into the TUI —
// everything that arrives is literal text.
const CONTROL_RE = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g; // eslint-disable-line no-control-regex

const bad = msg => Object.assign(new Error(msg), { status: 400 });

/** Validate and normalize a send-keys request body. Throws with .status 400. */
export function parseSendKeys(body) {
  const name = body?.name;
  if (typeof name !== 'string' || !NAME_RE.test(name)) throw bad(`bad name: ${name}`);

  const raw = body?.text;
  if (typeof raw !== 'string') throw bad('text required');
  if (raw.length > MAX_TEXT) throw bad('text too long');

  // The TUI submits at the first newline, so a multi-line paste would fire the
  // first line and scatter the rest into whatever prompt came next. Collapse to
  // one line and let the caller append a single Enter.
  const text = raw.replace(/[\r\n]+/g, ' ').replace(CONTROL_RE, '').trim();
  if (!text) throw bad('empty text');

  return { name, text };
}

/**
 * tmux argv for typing `text` into `name`, then submitting.
 *
 * Two commands because -l sends its operand literally: "Enter" typed literally
 * is the five characters, not the key. The `--` is load-bearing — without it
 * tmux reads a leading-dash command (`--version`) as its own flag and errors.
 */
export function sendKeysArgs({ name, text }) {
  return [
    ['send-keys', '-t', name, '-l', '--', text],
    ['send-keys', '-t', name, 'Enter'],
  ];
}

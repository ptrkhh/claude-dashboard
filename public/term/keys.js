/* Terminal key-bar bytes and sticky Ctrl, free of DOM so node --test can
   exercise them. Loaded as a classic script by index.html; the IIFE form makes
   the same file a valid side-effect ES module under node --test. */
(function (g) {
  g.cdashKeys = {
    KEYS: {
      esc: '\x1b', tab: '\t', stab: '\x1b[Z', pgup: '\x1b[5~', pgdn: '\x1b[6~',
      left: '\x1b[D', up: '\x1b[A', down: '\x1b[B', right: '\x1b[C',
      ctrlc: '\x03', enter: '\r',
    },
    /* Ctrl+<c> for a single character that has a C0 control code: @, letters,
       [ \ ] ^ _. Anything else, including a multi-character IME commit,
       passes through unchanged. */
    applyCtrl(data) {
      const c = data.length === 1 ? data.charCodeAt(0) : -1;
      const has = (c >= 0x40 && c <= 0x5f) || (c >= 0x61 && c <= 0x7a);
      return has ? String.fromCharCode(c & 0x1f) : data;
    },
  };
})(typeof globalThis === 'undefined' ? this : globalThis);

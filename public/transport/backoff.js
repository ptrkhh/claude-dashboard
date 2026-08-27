/* Poll-interval decision logic, deliberately free of DOM so node --test can
   exercise it. Loaded as a classic script by index.html; the IIFE form makes
   the same file a valid side-effect ES module under node --test. */
(function (g) {
  const LADDER = [4000, 8000, 15000, 30000];
  g.cdashBackoff = {
    initial: () => ({ i: 0, halted: false }),
    next(state, outcome) {
      if (outcome === 'ok') return { i: 0, halted: false };
      if (state.halted) return { ...state };
      if (outcome === 'auth') return { i: 0, halted: true };
      return { i: Math.min(state.i + 1, LADDER.length - 1), halted: false };
    },
    delay: state => LADDER[state.i],
    /* The halt rule lived inline in app.js where no test could see it. Only
       401/403 are terminal — the user must act. Throttling (429) and every
       other failure are transient and must back off, never halt. */
    outcomeFor: status => (status === 401 || status === 403 ? 'auth' : 'fail'),
  };
})(typeof globalThis === 'undefined' ? this : globalThis);

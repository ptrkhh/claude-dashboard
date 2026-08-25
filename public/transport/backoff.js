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
  };
})(typeof globalThis === 'undefined' ? this : globalThis);

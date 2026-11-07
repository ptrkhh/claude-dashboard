/* Browser-view helpers, free of DOM so node --test can exercise them. Loaded as
   a classic script by index.html; the IIFE form makes the same file a valid
   side-effect ES module under node --test. */
(function (g) {
  g.cdashBrowser = {
    /* Key-bar button to the named key the agent accepts. */
    KEYBAR: { tab: 'Tab', bksp: 'Backspace', left: 'ArrowLeft', right: 'ArrowRight', esc: 'Escape', enter: 'Enter' },
    /* x,y are relative to the transformed image's top-left, `shown` is its
       displayed size, `meta` the metadata paired with that JPEG. The frame's
       coordinates already start at the image, so offsetTop and pageScaleFactor
       are not applied. */
    toPage(x, y, shown, meta) {
      return [x / shown.w * meta.deviceWidth, y / shown.h * meta.deviceHeight];
    },
    /* Committed input only; partial composition (insertCompositionText) is
       handled at compositionend by the caller. */
    fromInput(inputType, data) {
      if (inputType === 'insertText' || inputType === 'insertFromPaste' || inputType === 'insertReplacementText') {
        return data ? [{ text: data }] : [];
      }
      if (inputType === 'deleteContentBackward') return [{ key: 'Backspace' }];
      if (inputType === 'insertLineBreak') return [{ key: 'Enter' }];
      return [];
    },
  };
})(typeof globalThis === 'undefined' ? this : globalThis);

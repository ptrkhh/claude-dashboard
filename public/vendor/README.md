Vendored, unmodified except for removed `sourceMappingURL` lines. MIT, see LICENSE-xterm
(addon-fit ships under the same license from the same project).

- xterm.js, xterm.css: @xterm/xterm 5.5.0 (lib/xterm.js, css/xterm.css)
- addon-fit.js: @xterm/addon-fit 0.10.0 (lib/addon-fit.js)

Upgrade: `npm pack` the new versions, copy the same three files, and keep the
`embedded_asset` arms in crates/agent/src/http/serve.rs in step.

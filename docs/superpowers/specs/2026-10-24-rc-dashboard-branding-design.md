# RC Dashboard branding

The public product name is RC Dashboard, with the tagline “Remote control for Claude Code and Codex.” Replace the previous Claude-style sunburst with the approved pixel Clawd and refined S2 signal. This follows the design approved in the conversation on 24 October 2026.

## Appearance

The header and favicon follow the existing resolved app theme, including a saved manual override and live system-theme changes. Light mode uses P6 (paper `#F6EEE4`, rust `#B95D3D`, signal `#654A3A`, eyes `#3D241E`). Dark mode uses P1 (ink `#19202C`, apricot `#F4A16F`, signal `#FFF3D6`, eyes `#101720`). Keep the approved mascot and signal geometry.

Launcher icons need one fixed appearance across the current Android, Windows, macOS, Linux and PWA packaging paths. The user selected P1 (ink + apricot) as the fixed appearance for every launcher/install icon. Android/PWA masks must leave the mascot and signal visible.

## Approaches

Use a fixed launcher icon and switch the header/favicon through the existing theme logic. This fits the current Tauri PNG/ICO/ICNS pipeline and browser UI without adding platform-specific runtime code. The alternative is separate Apple Icon Composer, Windows package assets and Android monochrome theming; those do not provide a portable P1/P6 pair through the current packaging path.

## Scope and compatibility

Update product/window/PWA labels, the README introduction, visible bridge branding, and icon assets. Preserve `dev.cdash.app`, executable and crate names, repository URLs, configuration paths, storage keys, session identifiers and existing release artifact names. Preserve the rest of the UI palette. This PR does not rename the GitHub repository or publish a release.

## Verification

Exercise manual light/dark overrides, system changes and inaccessible local storage. Verify the rendered icons at small sizes, native asset formats and launcher masking. Run the Node and Rust suites and the configured CI builds. Distinguish browser/asset checks from installed-device validation.

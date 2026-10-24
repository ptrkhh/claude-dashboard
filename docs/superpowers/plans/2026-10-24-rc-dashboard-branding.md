# RC Dashboard Branding Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task by task.

**Goal:** Create a PR that applies the approved RC Dashboard name and Clawd/S2 icon across the web and packaged apps.

**Architecture:** Reuse the existing app theme resolver and Tauri icon-generation pipeline. Keep one fixed launcher palette; update favicon URLs whenever the resolved theme changes.

**Tech Stack:** HTML/CSS/JavaScript, Node tests, Rust/Tauri, SVG/PNG/ICO/ICNS.

**Spec:** `docs/superpowers/specs/2026-10-24-rc-dashboard-branding-design.md`

## Global Constraints

- Preserve app identifier `dev.cdash.app`, commands, settings paths and storage keys.
- Keep P6 light and P1 dark for the header and favicon.
- Preserve the user's current checkout and unrelated changes.
- Use P1 (ink + apricot) for every fixed launcher/install icon.
- Keep the approved geometry; fit Android/PWA launcher masks.

## Review Focus

- A manual theme override must win over system changes.
- Local storage denial must not stop app startup or icon updates.
- Embedded web assets must include every newly referenced icon.
- Maskable launcher crops must preserve the signal and mascot.
- Renaming the product must not change app identity, upgrade paths or configuration locations.

### Task 1: Apply public names and themed web icons

**Files:** `public/index.html`, `public/app.js`, `public/style.css`, `public/manifest.json`, `public/icon*.svg`, `public/icon*.png`, `test/frontend-guards.test.mjs`, `test/theme.test.mjs`, `crates/agent/src/http/routes.rs`, `crates/agent/src/collect/codex-remote.mjs`, `crates/tauri-app/tauri.conf.json`, `README.md`.

- [x] Add theme behavior tests for saved overrides, live system changes and denied local storage; run them and observe the missing favicon updates.
- [x] Apply the public name and tagline; reuse approved SVG sources for the header and favicon.
- [x] Wire favicon updates into `applyMode`; retain the existing theme/storage behavior.
- [x] Update the existing embedded-page title assertion; verify new assets are served with correct content types.
- [x] Run `node --test` and `cargo test --all --locked`; require passing results.

### Task 2: Generate launcher assets

**Files:** `crates/tauri-app/icons/*`, `public/manifest.json`, launcher PNGs, `scripts/release.sh` only if necessary for reproducible Android assets, and the branding documentation.

- [x] Record the user's fixed launcher palette in the spec.
- [x] Generate PNG/ICO/ICNS and Android launcher resources using the installed pinned Tauri CLI.
- [x] Provide an explicit maskable PWA icon with the artwork inside its safe region.
- [x] Inspect actual generated Android masks and fix the source/generation step if necessary.
- [x] Verify image dimensions, formats and small-size clarity; document the fixed/native versus themed/web behavior.

### Task 3: Validate and open the PR

- [x] Run relevant Rust/Node tests, Clippy, syntax checks and `git diff --check`.
- [x] Verify both themes in a browser, including manual overrides and system changes.
- [ ] Obtain a fresh whole-branch review; address material findings.
- [ ] Commit only this rebrand, push the feature branch and create the requested PR against current `master`.
- [ ] Verify PR scope and CI status; report the PR URL and any unrun platform checks.

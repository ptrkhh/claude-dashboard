# PR49 container improvements implementation plan

**Goal:** Make the public Docker setup reliable without importing a personal workstation.
**Spec:** The reviewed PR49 todo list approved in this conversation on 2026-11-08.
**Architecture:** Keep the two-service Compose stack and existing authentication. Improve the image, startup readiness, build caching, and public onboarding.
**Tech stack:** Docker BuildKit, Compose, Debian bookworm, Node 22, Rust 1.94.1, Bash.

## Constraints

- Work on the existing `feat/docker-image` PR branch and commit the changes.
- Preserve Cloudflare Access and keep port 23274 unpublished.
- Keep the agent running as node; allow the requested passwordless sudo.
- Keep OpenCode on v2. Pin CLI defaults through build arguments.
- Defer timezone configuration, personal toolchains, and host configuration imports.
- Do not modify unrelated user files.

## Review focus

- A cache-mounted target directory must still export a binary to the runtime stage.
- A differently owned mounted repository must support Git and ordinary file writes.
- An unauthenticated healthcheck must work while dashboard routes remain guarded.
- Removing a container must preserve the named home volume.
- The default projects directory must stay out of the build context and Git.

## Task 1: Behavioral verification

- [x] Add `scripts/test-docker.sh` to exercise the real built image: health, bearer guard, non-root default, sudo, CLIs, differently owned repository, writes, and home persistence.
- [x] Add a Docker CI job that validates Compose, checks build-context exclusion, builds the runtime image, and tests a rebuild after a source change.
- [x] Validate shell syntax locally.
- [ ] Run image checks in Docker CI; the local environment has no Docker CLI or daemon.

## Task 2: Image and Compose

- [x] Cache Cargo registry, Git, and target directories, use `--locked`, and export the binary outside the target cache.
- [x] Install sudo/unzip, validate sudoers, and configure system Git trust.
- [x] Pin Claude Code 2.1.319, Codex 0.167.0, OpenCode 2.2.6, and cloudflared 2026.10.7.
- [x] Add an image healthcheck and gate cloudflared on service_healthy.
- [x] Ignore projects/ in Docker and Git.

## Task 3: Documentation and completion

- [x] Clarify the HTTPS issuer, fresh CLI settings, volume persistence, package-install lifetime, sudo, and mounted-repository trust.
- [x] Explain separate independent tunnels and stopping an old project while retaining its home volume.
- [x] Correct the container project path and state Linux write-permission requirements.
- [x] Run the Node suite, inspect the complete diff, get a fresh review, and commit the approved files.

# syntax=docker/dockerfile:1

# Both stages are Debian bookworm so the glibc-linked binary runs as built.
FROM rust:1.94.1-bookworm AS build
# The default 30s idle timeout is short enough that a slow crates.io mirror
# fails the build outright.
ENV CARGO_NET_RETRY=10 CARGO_HTTP_TIMEOUT=120
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=${CARGO_HOME}/registry \
    --mount=type=cache,target=${CARGO_HOME}/git \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p cdash-agent \
    && mkdir -p /out \
    && cp target/release/cdash-agent /out/cdash-agent

FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends tmux git curl ca-certificates sudo unzip procps \
    && printf '%s\n' 'node ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/node \
    && chmod 0440 /etc/sudoers.d/node \
    && visudo -cf /etc/sudoers \
    && git config --system --add safe.directory '*' \
    && rm -rf /var/lib/apt/lists/*

ARG CLAUDE_CODE_VERSION=2.1.319
ARG CODEX_VERSION=0.167.0
# OpenCode v2 only: the dashboard rejects v1 and unknown future majors.
ARG OPENCODE_VERSION=2.2.6
RUN npm install -g \
        "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
        "@openai/codex@${CODEX_VERSION}" \
        "@opencode/cli@${OPENCODE_VERSION}" \
    && claude --version \
    && codex --version \
    && opencode --version \
    && npm cache clean --force

# Cache mounts are not image layers; /out retains the finished binary.
COPY --from=build /out/cdash-agent /usr/local/bin/cdash-agent

# Projects mount under the home volume, so the picker (which opens at $HOME)
# lists it first. The mountpoint must be owned by node.
RUN mkdir -p /home/node/projects && chown node:node /home/node/projects
ENV LANG=C.UTF-8 LC_ALL=C.UTF-8

USER node
WORKDIR /home/node/projects
EXPOSE 23274
HEALTHCHECK --interval=15s --timeout=3s --start-period=15s --retries=3 \
    CMD curl -fsS --max-time 2 --output /dev/null "http://127.0.0.1:${PORT:-23274}/api/health"
CMD ["cdash-agent"]

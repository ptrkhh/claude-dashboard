# syntax=docker/dockerfile:1

# Both stages are Debian bookworm so the glibc-linked binary runs as built.
FROM rust:1.94.1-bookworm AS build
# The default 30s idle timeout is short enough that a slow crates.io mirror
# fails the build outright.
ENV CARGO_NET_RETRY=10 CARGO_HTTP_TIMEOUT=120
WORKDIR /src
COPY . .
RUN cargo build --release -p cdash-agent

FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends tmux git curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# OpenCode v2 only: the dashboard rejects v1 and unknown future majors.
RUN npm install -g @anthropic-ai/claude-code @openai/codex @opencode/cli@2 \
    && npm cache clean --force

COPY --from=build /src/target/release/cdash-agent /usr/local/bin/cdash-agent

# Projects mount under the home volume, so the picker (which opens at $HOME)
# lists it first. The mountpoint must be owned by node.
RUN mkdir -p /home/node/projects && chown node:node /home/node/projects
ENV LANG=C.UTF-8 LC_ALL=C.UTF-8

USER node
WORKDIR /home/node/projects
EXPOSE 23274
CMD ["cdash-agent"]

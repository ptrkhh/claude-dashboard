#!/usr/bin/env bash
# Run against a built image; no CLI credentials or published ports are needed.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    printf 'Usage: bash scripts/test-docker.sh IMAGE\n' >&2
    exit 2
fi
if ! command -v docker >/dev/null 2>&1; then
    printf 'Docker is required for the container smoke test.\n' >&2
    exit 1
fi

image=$1
smoke_name="cdash-smoke-$(date +%s)-$$"
container="$smoke_name-tools"
home_volume="$smoke_name-home"
projects_volume="$smoke_name-projects"

cleanup() {
    smoke_status=$?
    trap - EXIT
    if [ "$smoke_status" -ne 0 ]; then
        docker logs "$container" >&2 || true
    fi
    docker rm -f "$container" >/dev/null 2>&1 || true
    docker volume rm "$home_volume" "$projects_volume" >/dev/null 2>&1 || true
    exit "$smoke_status"
}
trap cleanup EXIT

start_agent() {
    docker run -d --name "$container" \
        --env CDASH_AUTH=bearer --env CDASH_TOKEN=smoke-token \
        --env CDASH_BIND=0.0.0.0 \
        --mount "type=volume,src=$home_volume,dst=/home/node" \
        --mount "type=volume,src=$projects_volume,dst=/home/node/projects" \
        "$image" >/dev/null
    for attempt in $(seq 1 60); do
        if [ "$(docker inspect --format '{{.State.Health.Status}}' "$container")" = healthy ]; then
            return
        fi
        if [ "$(docker inspect --format '{{.State.Running}}' "$container")" != true ]; then
            break
        fi
        sleep 1
    done
    printf 'The image did not become healthy.\n' >&2
    return 1
}

start_agent

# Health is public, but a caller still needs the configured guard elsewhere.
docker exec "$container" sh -ec '
    test "$(curl -fsS http://127.0.0.1:23274/api/health)" = "{\"ok\":true}"
    test "$(curl -sS -o /dev/null -w "%{http_code}" http://127.0.0.1:23274/)" = 401
    curl -fsS -H "Authorization: Bearer smoke-token" http://127.0.0.1:23274/ >/dev/null
    test "$(id -u)" -ne 0
    test "$(sudo -n id -u)" = 0
    claude --version
    codex --version
    opencode --version
    unzip -v >/dev/null
    printf "persisted\n" > /home/node/.cdash-smoke-state
'

# A writable mount owned by a different UID must not break Git for node.
docker exec --user root "$container" sh -ec '
    mkdir /home/node/projects/smoke-repo
    git init -q /home/node/projects/smoke-repo
    chmod -R a+rwX /home/node/projects/smoke-repo
'
docker exec "$container" sh -ec '
    test "$(stat -c %u /home/node/projects/smoke-repo/.git)" = 0
    git -C /home/node/projects/smoke-repo status --porcelain
    printf "writable\n" > /home/node/projects/smoke-repo/write-check
'

# Recreate the container without deleting its volumes.
docker rm -f "$container" >/dev/null
start_agent
docker exec "$container" sh -ec '
    test "$(cat /home/node/.cdash-smoke-state)" = persisted
    test "$(cat /home/node/projects/smoke-repo/write-check)" = writable
    git -C /home/node/projects/smoke-repo status --porcelain
'
printf 'Container smoke checks passed.\n'
